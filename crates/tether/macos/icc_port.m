// libgphoto2 port driver over macOS ImageCaptureCore (notes: reference/tether.md).
//
// macOS's camera service (ptpcamerad) owns every PTP camera on USB; claiming
// the device with libusb fights it. ImageCaptureCore instead passes PTP
// transactions through that service (ICCameraDevice
// requestSendPTPCommand:outData:completion:), as Capture One does. This
// driver presents each ICC camera that accepts PTP commands as a USB port,
// so libgphoto2's PTP driver keeps all vendor protocol logic and only the
// transport changes:
//
//   write  command container, then the data-out container(s): buffered;
//   read   the first read after a command runs it through ICC, then serves
//          the data-in container and the response container, one container
//          per read like USB transfers, with ptp2's transaction id;
//   interrupt endpoint  the camera's PTP events, as ICC reports them.
//
// OpenSession/CloseSession are answered here: ICC owns the camera's session.
// A camera ICC removed reports "device not found" (GP_ERROR_IO_USB_FIND).
//
// ICC delivers its callbacks on the main thread: the process must run the
// main run loop and call this driver from other threads (rawtonic-tether
// does). Nothing here deletes or changes anything on the camera by itself.

#import <Foundation/Foundation.h>
#import <ImageCaptureCore/ImageCaptureCore.h>

#include <gphoto2/gphoto2-port-library.h>
#include <gphoto2/gphoto2-port-log.h>
#include <gphoto2/gphoto2-port-result.h>
#include <gphoto2/gphoto2-port.h>
#include <dlfcn.h>
#include <string.h>

#define PTP_CONTAINER_COMMAND 1
#define PTP_CONTAINER_DATA 2
#define PTP_CONTAINER_RESPONSE 3
#define PTP_OC_OPEN_SESSION 0x1002
#define PTP_OC_CLOSE_SESSION 0x1003
#define PTP_RC_OK 0x2001
// How long one transaction may take before the port gives up (a RAW
// download over USB 2 included).
#define TRANSACTION_SECONDS 60
// How long the first look for cameras waits for ICC's list of local devices.
#define ENUMERATION_SECONDS 3
// After that: rounds of this many ms until no new camera came.
#define SETTLE_MS 100
#define SETTLE_ROUNDS 10

// libgphoto2 unloads a port driver after listing ports and loads it again
// to open one; Objective-C classes and ICC's callbacks must not see this
// image unmapped, so it stays loaded once loaded.
__attribute__((constructor)) static void stay_loaded(void) {
    Dl_info self;
    if (dladdr((const void *)stay_loaded, &self) && self.dli_fname) dlopen(self.dli_fname, RTLD_NOLOAD | RTLD_NODELETE);
}

static uint32_t le32(const uint8_t *p) { return p[0] | p[1] << 8 | p[2] << 16 | (uint32_t)p[3] << 24; }
static uint16_t le16(const uint8_t *p) { return p[0] | p[1] << 8; }
static void put32(uint8_t *p, uint32_t v) { p[0] = v; p[1] = v >> 8; p[2] = v >> 16; p[3] = v >> 24; }
static void put16(uint8_t *p, uint16_t v) { p[0] = v; p[1] = v >> 8; }

// One camera as this process sees it: its ICC device, a stable port number,
// its PTP events and whether ICC still has it.
@interface RTCamera : NSObject <ICCameraDeviceDelegate>
@property(strong) ICCameraDevice *device;
@property unsigned number;
@property BOOL removed;
@property(strong) NSCondition *eventsChanged;
@property(strong) NSMutableArray<NSData *> *events;
@end

@implementation RTCamera
- (instancetype)initWithDevice:(ICCameraDevice *)device number:(unsigned)number {
    if ((self = [super init])) {
        _device = device;
        _number = number;
        _eventsChanged = [NSCondition new];
        _events = [NSMutableArray new];
    }
    return self;
}
- (void)cameraDevice:(ICCameraDevice *)camera didReceivePTPEvent:(NSData *)eventData {
    [self.eventsChanged lock];
    [self.events addObject:eventData];
    [self.eventsChanged broadcast];
    [self.eventsChanged unlock];
}
- (void)gone {
    [self.eventsChanged lock];
    self.removed = YES;
    [self.eventsChanged broadcast];
    [self.eventsChanged unlock];
}
- (void)didRemoveDevice:(ICDevice *)device { [self gone]; }
- (void)device:(ICDevice *)device didOpenSessionWithError:(NSError *)error {}
- (void)device:(ICDevice *)device didCloseSessionWithError:(NSError *)error {}
- (void)deviceDidBecomeReadyWithCompleteContentCatalog:(ICCameraDevice *)device {}
- (void)cameraDevice:(ICCameraDevice *)camera didAddItems:(NSArray<ICCameraItem *> *)items {}
- (void)cameraDevice:(ICCameraDevice *)camera didRemoveItems:(NSArray<ICCameraItem *> *)items {}
- (void)cameraDevice:(ICCameraDevice *)camera didRenameItems:(NSArray<ICCameraItem *> *)items {}
- (void)cameraDevice:(ICCameraDevice *)camera
    didReceiveThumbnail:(CGImageRef)thumbnail
                forItem:(ICCameraItem *)item
                  error:(NSError *)error {}
- (void)cameraDevice:(ICCameraDevice *)camera
    didReceiveMetadata:(NSDictionary *)metadata
               forItem:(ICCameraItem *)item
                 error:(NSError *)error {}
- (void)cameraDeviceDidChangeCapability:(ICCameraDevice *)camera {}
- (void)cameraDeviceDidRemoveAccessRestriction:(ICDevice *)device {}
- (void)cameraDeviceDidEnableAccessRestriction:(ICDevice *)device {}
@end

// The process-wide browser and the cameras it found.
@interface RTBrowser : NSObject <ICDeviceBrowserDelegate>
@property(strong) ICDeviceBrowser *browser;
@property(strong) NSMutableArray<RTCamera *> *cameras;
@property unsigned nextNumber;
@property(strong) dispatch_semaphore_t enumerated;
@end

@implementation RTBrowser
- (instancetype)init {
    if ((self = [super init])) {
        _cameras = [NSMutableArray new];
        _nextNumber = 1;
        _enumerated = dispatch_semaphore_create(0);
    }
    return self;
}
- (void)deviceBrowser:(ICDeviceBrowser *)browser didAddDevice:(ICDevice *)device moreComing:(BOOL)moreComing {
    [self add:device];
}
// Main thread.
- (void)add:(ICDevice *)device {
    if (![device isKindOfClass:[ICCameraDevice class]]) return;
    @synchronized(self) {
        for (RTCamera *known in self.cameras)
            if (known.device == device) return;
        RTCamera *camera = [[RTCamera alloc] initWithDevice:(ICCameraDevice *)device number:self.nextNumber++];
        device.delegate = camera;
        [self.cameras addObject:camera];
    }
    gp_log(GP_LOG_DEBUG, "icc", "camera added: %s (%04x:%04x)", device.name.UTF8String, device.usbVendorID,
           device.usbProductID);
}
- (void)deviceBrowser:(ICDeviceBrowser *)browser didRemoveDevice:(ICDevice *)device moreGoing:(BOOL)moreGoing {
    @synchronized(self) {
        for (RTCamera *camera in [self.cameras copy]) {
            if (camera.device == device) {
                [camera gone];
                [self.cameras removeObject:camera];
            }
        }
    }
    gp_log(GP_LOG_DEBUG, "icc", "camera removed: %s", device.name.UTF8String);
}
- (void)deviceBrowserDidEnumerateLocalDevices:(ICDeviceBrowser *)browser {
    dispatch_semaphore_signal(self.enumerated);
}
- (NSArray<RTCamera *> *)ptpCameras {
    NSMutableArray *found = [NSMutableArray new];
    @synchronized(self) {
        for (RTCamera *camera in self.cameras)
            if ([camera.device.capabilities containsObject:ICCameraDeviceCanAcceptPTPCommands])
                [found addObject:camera];
    }
    return found;
}
@end

static RTBrowser *shared_browser(void) {
    static RTBrowser *browser;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
      browser = [RTBrowser new];
      dispatch_async(dispatch_get_main_queue(), ^{
        browser.browser = [ICDeviceBrowser new];
        browser.browser.delegate = browser;
        browser.browser.browsedDeviceTypeMask =
            (ICDeviceTypeMask)(ICDeviceTypeMaskCamera | ICDeviceLocationTypeMaskLocal);
        [browser.browser start];
      });
      if (dispatch_semaphore_wait(browser.enumerated,
                                  dispatch_time(DISPATCH_TIME_NOW, ENUMERATION_SECONDS * NSEC_PER_SEC))) {
          gp_log(GP_LOG_ERROR, "icc", "ImageCaptureCore did not list the local cameras (main run loop not running?)");
          return;
      }
      // ICC reports the end of its enumeration before it hands over the
      // devices it found (~20 ms later on the a7 III): wait until no device
      // has come for a while.
      for (int i = 0; i < SETTLE_ROUNDS; i++) {
          NSUInteger before;
          @synchronized(browser) { before = browser.cameras.count; }
          usleep(SETTLE_MS * 1000);
          dispatch_sync(dispatch_get_main_queue(), ^{
            for (ICDevice *device in browser.browser.devices) [browser add:device];
          });
          NSUInteger after;
          @synchronized(browser) { after = browser.cameras.count; }
          if (after == before && (after > 0 || i >= 2)) break;
      }
    });
    return browser;
}

// Port paths are usb:BBB,NNN: BBB = the USB bus from ICC's location id,
// NNN = the camera's number in this process (stable while it stays connected).
static NSString *path_of(RTCamera *camera) {
    return [NSString stringWithFormat:@"usb:%03u,%03u", (camera.device.usbLocationID >> 24) & 0xff, camera.number];
}

// What one open port keeps: its camera and the transaction in flight.
@interface RTPort : NSObject
@property(strong) RTCamera *camera;
@property BOOL session;
@property(strong) NSMutableData *out;            // command + data-out bytes not yet run
@property(strong) NSMutableArray<NSData *> *in;  // containers to be read, oldest first
@property NSUInteger inOffset;                   // bytes of in[0] already read
@end

@implementation RTPort
- (instancetype)init {
    if ((self = [super init])) [self reset];
    return self;
}
- (void)reset {
    self.out = [NSMutableData new];
    self.in = [NSMutableArray new];
    self.inOffset = 0;
}
@end

struct _GPPortPrivateLibrary {
    void *port;  // RTPort, retained
};

static RTPort *state(GPPort *port) { return port->pl ? (__bridge RTPort *)port->pl->port : nil; }
static RTCamera *camera_of(GPPort *port) { return state(port).camera; }

GPPortType gp_port_library_type(void) { return GP_PORT_USB; }

int gp_port_library_list(GPPortInfoList *list) {
    if ([NSThread isMainThread]) {
        gp_log(GP_LOG_ERROR, "icc", "called on the main thread, which must run ImageCaptureCore's run loop");
        return GP_ERROR;
    }
    GPPortInfo info;
    // Any camera: "usb:" alone.
    if (gp_port_info_new(&info) < 0) return GP_ERROR_NO_MEMORY;
    gp_port_info_set_type(info, GP_PORT_USB);
    gp_port_info_set_name(info, "");
    gp_port_info_set_path(info, "^usb:");
    gp_port_info_list_append(list, info);
    for (RTCamera *camera in [shared_browser() ptpCameras]) {
        if (gp_port_info_new(&info) < 0) return GP_ERROR_NO_MEMORY;
        gp_port_info_set_type(info, GP_PORT_USB);
        gp_port_info_set_name(info, "Universal Serial Bus");
        gp_port_info_set_path(info, path_of(camera).UTF8String);
        if (gp_port_info_list_append(list, info) < 0) return GP_ERROR;
    }
    return GP_OK;
}

static int icc_init(GPPort *port) {
    port->pl = calloc(1, sizeof(GPPortPrivateLibrary));
    if (!port->pl) return GP_ERROR_NO_MEMORY;
    port->pl->port = (void *)CFBridgingRetain([RTPort new]);
    return GP_OK;
}

static int icc_exit(GPPort *port) {
    if (!port->pl) return GP_OK;
    if (port->pl->port) CFBridgingRelease(port->pl->port);
    free(port->pl);
    port->pl = NULL;
    return GP_OK;
}

// The camera this port's path names ("usb:" alone: the first one matching).
static RTCamera *resolve(GPPort *port, BOOL (^matches)(RTCamera *)) {
    GPPortInfo info;
    char *path = NULL;
    if (gp_port_get_info(port, &info) < 0 || gp_port_info_get_path(info, &path) < 0 || !path) return nil;
    NSString *wanted = [NSString stringWithUTF8String:path];
    for (RTCamera *camera in [shared_browser() ptpCameras]) {
        BOOL named = [wanted isEqualToString:@"usb:"] || [wanted isEqualToString:path_of(camera)];
        if (named && matches(camera)) return camera;
    }
    return nil;
}

static int attach(GPPort *port, RTCamera *camera) {
    if (!camera) {
        gp_port_set_error(port, "No such camera on USB (ImageCaptureCore).");
        return GP_ERROR_IO_USB_FIND;
    }
    state(port).camera = camera;
    // ptp2 reads these; ImageCaptureCore hides the endpoints.
    port->settings.usb.inep = 0x81;
    port->settings.usb.outep = 0x02;
    port->settings.usb.intep = 0x83;
    port->settings.usb.config = 1;
    port->settings.usb.interface = 0;
    port->settings.usb.altsetting = 0;
    port->settings.usb.maxpacketsize = 512;
    strlcpy(port->settings.usb.port, path_of(camera).UTF8String, sizeof(port->settings.usb.port));
    return GP_OK;
}

static int icc_find_device(GPPort *port, int vendor, int product) {
    return attach(port, resolve(port, ^BOOL(RTCamera *c) {
                      return c.device.usbVendorID == vendor && c.device.usbProductID == product;
                  }));
}

static int icc_find_device_by_class(GPPort *port, int class, int subclass, int protocol) {
    // ImageCaptureCore lists still-image (PTP, class 6) cameras only.
    if (class != 6) return GP_ERROR_IO_USB_FIND;
    return attach(port, resolve(port, ^BOOL(RTCamera *c) { return YES; }));
}

// Waits for an ICC completion; NO when it did not come in time.
static BOOL wait_for(dispatch_semaphore_t done, int seconds) {
    return dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, (int64_t)seconds * NSEC_PER_SEC)) == 0;
}

static int icc_open(GPPort *port) {
    RTCamera *camera = camera_of(port);
    if (!camera) {
        int r = icc_find_device_by_class(port, 6, 0, 0);
        if (r < 0) return r;
        camera = camera_of(port);
    }
    if (camera.removed) return GP_ERROR_IO_USB_FIND;
    [state(port) reset];
    [camera.eventsChanged lock];
    [camera.events removeAllObjects];
    [camera.eventsChanged unlock];
    __block NSError *failed = nil;
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    dispatch_async(dispatch_get_main_queue(), ^{
      [camera.device requestOpenSessionWithOptions:@{}
                                 completion:^(NSError *error) {
                                   failed = error;
                                   dispatch_semaphore_signal(done);
                                 }];
    });
    if (!wait_for(done, 20)) {
        gp_port_set_error(port, "The camera did not open a session (ImageCaptureCore).");
        return GP_ERROR_TIMEOUT;
    }
    if (failed) {
        gp_port_set_error(port, "Opening the camera failed: %s", failed.localizedDescription.UTF8String);
        return GP_ERROR_IO_USB_CLAIM;
    }
    state(port).session = YES;
    return GP_OK;
}

static int icc_close(GPPort *port) {
    RTCamera *camera = camera_of(port);
    if (!camera || !state(port).session) return GP_OK;
    state(port).session = NO;
    if (camera.removed) return GP_OK;
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    dispatch_async(dispatch_get_main_queue(), ^{
      [camera.device requestCloseSessionWithOptions:@{}
                                  completion:^(NSError *error) {
                                    dispatch_semaphore_signal(done);
                                  }];
    });
    wait_for(done, 10);
    return GP_OK;
}

static int icc_write(GPPort *port, const char *bytes, int size) {
    if (!camera_of(port)) return GP_ERROR_BAD_PARAMETERS;
    if (camera_of(port).removed) return GP_ERROR_IO_USB_FIND;
    if (size > 0) [state(port).out appendBytes:bytes length:size];
    return size;
}

static NSData *container(uint16_t type, uint16_t code, uint32_t transaction, const void *payload, NSUInteger length) {
    NSMutableData *data = [NSMutableData dataWithLength:12];
    uint8_t *h = data.mutableBytes;
    put32(h, (uint32_t)(12 + length));
    put16(h + 4, type);
    put16(h + 6, code);
    put32(h + 8, transaction);
    if (length) [data appendBytes:payload length:length];
    return data;
}

// Runs the buffered transaction through ICC and queues what the camera
// answered.
static int run_transaction(GPPort *port) {
    RTCamera *camera = camera_of(port);
    RTPort *p = state(port);
    NSData *out = p.out;
    p.out = [NSMutableData new];
    const uint8_t *b = out.bytes;
    if (out.length < 12 || le16(b + 4) != PTP_CONTAINER_COMMAND || le32(b) > out.length || le32(b) < 12) {
        gp_port_set_error(port, "No PTP command to send (%lu bytes buffered).", (unsigned long)out.length);
        return GP_ERROR_IO_READ;
    }
    uint32_t commandLength = le32(b);
    uint16_t code = le16(b + 6);
    uint32_t transaction = le32(b + 8);
    // Data-out: the payload of the data containers after the command.
    NSMutableData *payload = nil;
    NSUInteger at = commandLength;
    while (at + 12 <= out.length) {
        uint32_t length = le32(b + at);
        if (le16(b + at + 4) != PTP_CONTAINER_DATA || length < 12) break;
        if (!payload) payload = [NSMutableData new];
        NSUInteger end = MIN(at + length, out.length);
        [payload appendBytes:b + at + 12 length:end - at - 12];
        at = end;
    }
    NSData *command = [out subdataWithRange:NSMakeRange(0, commandLength)];

    if (code == PTP_OC_OPEN_SESSION || code == PTP_OC_CLOSE_SESSION) {
        [p.in addObject:container(PTP_CONTAINER_RESPONSE, PTP_RC_OK, transaction, NULL, 0)];
        return GP_OK;
    }
    if (camera.removed) return GP_ERROR_IO_USB_FIND;
    __block NSData *dataIn = nil, *response = nil;
    __block NSError *failed = nil;
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    dispatch_async(dispatch_get_main_queue(), ^{
      [camera.device requestSendPTPCommand:command
                                   outData:payload
                                completion:^(NSData *data, NSData *ptpResponse, NSError *error) {
                                  dataIn = data;
                                  response = ptpResponse;
                                  failed = error;
                                  dispatch_semaphore_signal(done);
                                }];
    });
    if (!wait_for(done, TRANSACTION_SECONDS)) {
        gp_port_set_error(port, "The camera did not answer PTP operation 0x%04x.", code);
        return GP_ERROR_TIMEOUT;
    }
    if (camera.removed) return GP_ERROR_IO_USB_FIND;
    if (failed || response.length < 12) {
        gp_port_set_error(port, "PTP operation 0x%04x failed: %s", code,
                          failed ? failed.localizedDescription.UTF8String : "no response");
        return GP_ERROR_IO_READ;
    }
    if (dataIn.length)
        [p.in addObject:container(PTP_CONTAINER_DATA, code, transaction, dataIn.bytes, dataIn.length)];
    // The response as the camera sent it, with ptp2's transaction id (ICC
    // numbers transactions itself).
    NSMutableData *r = [response mutableCopy];
    uint32_t rlength = MIN(le32(r.bytes), (uint32_t)r.length);
    if (rlength < 12) rlength = (uint32_t)r.length;
    r.length = rlength;
    put32((uint8_t *)r.mutableBytes + 8, transaction);
    [p.in addObject:r];
    return GP_OK;
}

static int icc_read(GPPort *port, char *bytes, int size) {
    RTPort *p = state(port);
    if (!p.camera) return GP_ERROR_BAD_PARAMETERS;
    if (p.in.count == 0) {
        if (p.out.length == 0) {
            gp_port_set_error(port, "Nothing to read.");
            return GP_ERROR_TIMEOUT;
        }
        int r = run_transaction(port);
        if (r < 0) return r;
    }
    // One container per read, as a USB transfer ends at a short packet.
    NSData *next = p.in.firstObject;
    NSUInteger n = MIN(next.length - p.inOffset, (NSUInteger)size);
    memcpy(bytes, (const uint8_t *)next.bytes + p.inOffset, n);
    p.inOffset += n;
    if (p.inOffset >= next.length) {
        [p.in removeObjectAtIndex:0];
        p.inOffset = 0;
    }
    return (int)n;
}

static int icc_check_int(GPPort *port, char *bytes, int size, int timeout) {
    RTCamera *camera = camera_of(port);
    if (!camera) return GP_ERROR_BAD_PARAMETERS;
    NSDate *until = [NSDate dateWithTimeIntervalSinceNow:timeout / 1000.0];
    [camera.eventsChanged lock];
    while (camera.events.count == 0 && !camera.removed)
        if (![camera.eventsChanged waitUntilDate:until]) break;
    NSData *event = camera.events.firstObject;
    if (event) [camera.events removeObjectAtIndex:0];
    BOOL removed = camera.removed;
    [camera.eventsChanged unlock];
    if (!event) return removed ? GP_ERROR_IO_USB_FIND : GP_ERROR_TIMEOUT;
    int n = (int)MIN(event.length, (NSUInteger)size);
    memcpy(bytes, event.bytes, n);
    return n;
}

static int icc_ok(GPPort *port) { return GP_OK; }
static int icc_clear_halt(GPPort *port, int ep) { return GP_OK; }

// PTP class requests: 0x67 Get Device Status answers "OK"; the others
// (cancel, reset) have no equivalent through ICC.
static int icc_msg_class_read(GPPort *port, int request, int value, int index, char *bytes, int size) {
    if (request == 0x67 && size >= 4) {
        put16((uint8_t *)bytes, 4);
        put16((uint8_t *)bytes + 2, PTP_RC_OK);
        return 4;
    }
    return GP_ERROR_NOT_SUPPORTED;
}
static int icc_msg_class_write(GPPort *port, int request, int value, int index, char *bytes, int size) {
    return GP_OK;
}
static int icc_unsupported(GPPort *port, int request, int value, int index, char *bytes, int size) {
    return GP_ERROR_NOT_SUPPORTED;
}

GPPortOperations *gp_port_library_operations(void) {
    GPPortOperations *ops = calloc(1, sizeof(GPPortOperations));
    if (!ops) return NULL;
    ops->init = icc_init;
    ops->exit = icc_exit;
    ops->open = icc_open;
    ops->close = icc_close;
    ops->read = icc_read;
    ops->write = icc_write;
    ops->check_int = icc_check_int;
    ops->update = icc_ok;
    ops->reset = icc_ok;
    ops->find_device = icc_find_device;
    ops->find_device_by_class = icc_find_device_by_class;
    ops->clear_halt = icc_clear_halt;
    ops->msg_write = icc_unsupported;
    ops->msg_read = icc_unsupported;
    ops->msg_interface_write = icc_unsupported;
    ops->msg_interface_read = icc_unsupported;
    ops->msg_class_write = icc_msg_class_write;
    ops->msg_class_read = icc_msg_class_read;
    return ops;
}
