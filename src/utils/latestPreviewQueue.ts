/** Up to `maxInFlight` active renders and one replaceable pending state. A
 * released slider goes through the same queue, so an older drag can never run
 * after its final value (with more than one in flight, the renderer must keep
 * submission order). */
export class LatestPreviewQueue<T> {
  private running = 0;
  private pending: T | null = null;
  /** Counters for profiling: values submitted, replaced while still pending
   * (never rendered) and handed to the renderer. */
  readonly stats = { submitted: 0, superseded: 0, dispatched: 0 };

  constructor(
    private readonly render: (value: T) => Promise<void>,
    private readonly maxInFlight = 1,
  ) {}

  submit(value: T) {
    this.stats.submitted++;
    if (this.pending !== null) this.stats.superseded++;
    this.pending = value;
    this.flush();
  }

  clear() {
    this.pending = null;
  }

  private flush() {
    if (this.running >= this.maxInFlight || this.pending === null) return;
    const next = this.pending;
    this.pending = null;
    this.running++;
    this.stats.dispatched++;
    const finish = () => {
      this.running--;
      // Do not spend another animation frame waiting to dispatch the latest input.
      this.flush();
    };
    // The render adapter reports errors; a failed frame must not stall the queue.
    void this.render(next).then(finish, finish);
  }
}
