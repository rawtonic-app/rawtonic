//! Transfer a complete directory snapshot without a large UI-thread IPC result.
//! Scanning keeps the existing metadata/group semantics and runs on a worker.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::AppHandle;

use crate::file_management::{ImageFile, list_images_in_dir, list_images_recursive};

const PAGE_SIZE: usize = 512;
const MAX_SNAPSHOTS: usize = 4;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Serialize)]
pub struct ListingPage {
    listing_id: Option<String>,
    offset: usize,
    total: usize,
    files: Vec<ImageFile>,
}

struct Snapshot {
    files: std::vec::IntoIter<ImageFile>,
    total: usize,
    offset: usize,
    touched: Instant,
}

impl Snapshot {
    fn new(files: Vec<ImageFile>) -> Self {
        Self {
            total: files.len(),
            files: files.into_iter(),
            offset: 0,
            touched: Instant::now(),
        }
    }

    fn page(&mut self, id: &str) -> ListingPage {
        let offset = self.offset;
        let files: Vec<_> = self.files.by_ref().take(PAGE_SIZE).collect();
        self.offset += files.len();
        self.touched = Instant::now();
        ListingPage {
            listing_id: (self.offset < self.total).then(|| id.to_owned()),
            offset,
            total: self.total,
            files,
        }
    }
}

static SNAPSHOTS: LazyLock<Mutex<HashMap<String, Snapshot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[tauri::command]
pub async fn begin_image_listing(
    path: String,
    recursive: bool,
    app_handle: AppHandle,
) -> Result<ListingPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let files = if recursive {
            list_images_recursive(path, app_handle)?
        } else {
            list_images_in_dir(path, app_handle)?
        };
        let id = uuid::Uuid::new_v4().to_string();
        let mut snapshot = Snapshot::new(files);
        let first = snapshot.page(&id);
        if first.listing_id.is_some() {
            let mut snapshots = SNAPSHOTS.lock().map_err(|e| e.to_string())?;
            snapshots.retain(|_, entry| entry.touched.elapsed() < IDLE_TIMEOUT);
            // Never silently evict an active transfer. Normal browsing uses one;
            // extra slots permit overlapping cancellation/navigation responses.
            if snapshots.len() >= MAX_SNAPSHOTS {
                return Err(
                    "Too many active library listings; retry after the previous load finishes"
                        .into(),
                );
            }
            snapshots.insert(id, snapshot);
        }
        Ok(first)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn next_image_listing_page(listing_id: String, offset: usize) -> Result<ListingPage, String> {
    let mut snapshots = SNAPSHOTS.lock().map_err(|e| e.to_string())?;
    let snapshot = snapshots
        .get_mut(&listing_id)
        .ok_or("Library listing expired or was cancelled")?;
    if snapshot.offset != offset {
        return Err("Library listing page is out of sequence".into());
    }
    let page = snapshot.page(&listing_id);
    if page.listing_id.is_none() {
        snapshots.remove(&listing_id);
    }
    Ok(page)
}

#[tauri::command]
pub fn cancel_image_listing(listing_id: String) -> Result<(), String> {
    SNAPSHOTS
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&listing_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(count: usize) -> Vec<ImageFile> {
        (0..count)
            .map(|i| {
                serde_json::from_value(serde_json::json!({
                    "path": format!("/qa/{i}.dng"), "modified": i, "is_edited": i % 2 == 0,
                    "rating": i % 6, "tags": ["user:fixture"], "exif": null,
                    "is_virtual_copy": false, "is_cloud_placeholder": false, "is_raw": true,
                    "group_id": format!("group-{i}")
                }))
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn bounded_pages_preserve_complete_order_and_metadata() {
        let expected = files(PAGE_SIZE * 3 + 7);
        let mut snapshot = Snapshot::new(expected.clone());
        let mut actual = Vec::new();
        loop {
            let page = snapshot.page("fixture");
            assert_eq!(page.offset, actual.len());
            assert_eq!(page.total, expected.len());
            assert!(page.files.len() <= PAGE_SIZE);
            actual.extend(page.files);
            if page.listing_id.is_none() {
                break;
            }
        }
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(snapshot.files.len(), 0);
    }

    #[test]
    fn empty_and_single_page_results_need_no_retained_snapshot() {
        for count in [0, 1, PAGE_SIZE] {
            let page = Snapshot::new(files(count)).page("fixture");
            assert_eq!(page.total, count);
            assert_eq!(page.files.len(), count);
            assert!(page.listing_id.is_none());
        }
    }
}
