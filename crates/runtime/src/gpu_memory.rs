//! The error GPU work reports when the graphics card has no memory left
//! for it (a wgpu `OutOfMemory` caught by an error scope). Its text starts
//! every such error, whatever the pass, so an export can tell a lack of
//! graphics memory (often taken by another program for a while) from a
//! render that cannot work, and retry the photo. Render errors travel as
//! text through the export path; this marker is the one place that text
//! is defined.

/// The start of every GPU out-of-memory error.
pub const GPU_OUT_OF_MEMORY: &str = "The graphics card ran out of memory for this photograph.";

/// The error of a GPU pass (`what`) that ran out of graphics memory.
pub fn gpu_out_of_memory(what: &str) -> String {
    format!("{GPU_OUT_OF_MEMORY} Close other programs that use it and try again. ({what})")
}

/// Whether `error` is (or wraps) a GPU out-of-memory error.
pub fn is_gpu_out_of_memory(error: &str) -> bool {
    error.contains(GPU_OUT_OF_MEMORY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_out_of_memory_error_is_known_with_any_context_around_it() {
        let error = gpu_out_of_memory("the warp");
        assert!(is_gpu_out_of_memory(&error));
        assert!(is_gpu_out_of_memory(&format!(
            "a.CR3: not exported, {error}"
        )));
        assert!(!is_gpu_out_of_memory(
            "The graphics card could not render this photograph: Validation Error"
        ));
    }
}
