use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub type CallbackFn = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

#[derive(Clone)]
pub struct CallbackManager {
    callbacks: Arc<parking_lot::RwLock<HashMap<Vec<u8>, (CallbackFn, bool)>>>,
}

impl CallbackManager {
    pub fn new() -> Self {
        CallbackManager {
            callbacks: Arc::new(parking_lot::RwLock::new(HashMap::new())),
        }
    }

    /// Add a callback that will be triggered when marker is found in output
    /// Returns (marker_start, marker_end) - unique markers to embed in the stream
    pub fn add<F>(&self, id: usize,_marker_base: &str, callback: F, _one_shot: bool) -> (Vec<u8>, Vec<u8>)
    where
        F: Fn(Vec<u8>) + Send + Sync + 'static,
    {
        // let id = CALLBACK_ID_COUNTER.fetch_add(1, Ordering::SeqCst);

        let marker_start = format!("_POLYSH_{id}START_").into_bytes();
        let marker_end = format!("_POLYSH_{id}END_").into_bytes();

        let cb = Arc::new(callback);
        let mut cbs = self.callbacks.write();
        cbs.insert(marker_end.clone(), (cb, _one_shot));

        (marker_start, marker_end)
    }

    /// Process a line and check if it contains any markers
    /// Returns Some(remaining_data) if marker found, where remaining_data is content after marker
    /// Returns None if no marker found
    pub fn process(&self, line: &[u8]) -> Option<Vec<u8>> {
        // First, find if there's a marker (with read lock)
        let marker_to_process = {
            let callbacks = self.callbacks.read();
            let mut found: Option<(Vec<u8>, bool)> = None;
            for (marker, (_, one_shot)) in callbacks.iter() {
                if let Some(_) = line.windows(marker.len()).position(|w| w == marker.as_slice()) {
                    found = Some((marker.clone(), *one_shot));
                    break;
                }
            }
            found
        };

        // If found, process it
        if let Some((marker, one_shot)) = marker_to_process {
            // Call the callback
            {
                let callbacks = self.callbacks.read();
                if let Some((callback, _)) = callbacks.get(&marker) {
                    callback(line.to_vec());
                }
            }

            // Remove if one_shot
            if one_shot {
                let mut callbacks = self.callbacks.write();
                callbacks.remove(&marker);
            }

            // Return remaining content after marker
            if let Some(pos) = line.windows(marker.len()).position(|w| w == marker.as_slice()) {
                let remaining_start = pos + marker.len();
                let remaining = if remaining_start < line.len() {
                    line[remaining_start..].to_vec()
                } else {
                    Vec::new()
                };
                return Some(remaining);
            }
        }

        None
    }
    
}

impl Default for CallbackManager {
    fn default() -> Self {
        Self::new()
    }
}
