//! The AI tools' managed state (plan step 3.3). See `README.md` here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

pub struct AiTaskToken {
    cancelled: AtomicBool,
    notify: Notify,
}

impl AiTaskToken {
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub async fn wait_for_cancel(&self) {
        let notified = self.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

impl Default for AiTaskToken {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AiTaskGuard<'a> {
    active_ai_tasks: &'a Mutex<HashMap<String, Arc<AiTaskToken>>>,
    task_id: String,
    pub token: Arc<AiTaskToken>,
}

impl<'a> AiTaskGuard<'a> {
    pub fn new(
        active_ai_tasks: &'a Mutex<HashMap<String, Arc<AiTaskToken>>>,
        task_id: String,
    ) -> Self {
        let token = Arc::new(AiTaskToken::new());
        active_ai_tasks
            .lock()
            .unwrap()
            .insert(task_id.clone(), Arc::clone(&token));
        Self {
            active_ai_tasks,
            task_id,
            token,
        }
    }
}

impl<'a> Drop for AiTaskGuard<'a> {
    fn drop(&mut self) {
        let mut tasks = self.active_ai_tasks.lock().unwrap();
        if let Some(existing) = tasks.get(&self.task_id)
            && Arc::ptr_eq(existing, &self.token)
        {
            tasks.remove(&self.task_id);
        }
    }
}
