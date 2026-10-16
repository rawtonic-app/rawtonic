// Rawtonic patch (see RAWTONIC_PATCH.md), MIT like the rest of this crate.
//
//! The part of `proc-macro-error`'s API this crate used: entry points that
//! turn an abort into a compile error, `abort!` and `abort_call_site!`,
//! built on `syn::Error`. It replaces the unmaintained `proc-macro-error`
//! (RUSTSEC-2024-0370) and `proc-macro-error2` (RUSTSEC-2026-0173).

use proc_macro2::Span;
use quote::ToTokens;
use std::panic::{catch_unwind, panic_any, resume_unwind, AssertUnwindSafe};

/// Panic payload of an abort: the error to report.
struct Abort(syn::Error);

/// Stops the macro and reports `error` (caught by [`entry_point`]).
pub(crate) fn abort_with(error: syn::Error) -> ! {
    panic_any(Abort(error))
}

/// Runs a macro body; an abort inside it becomes its `compile_error!`.
pub(crate) fn entry_point(
    body: impl FnOnce() -> proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(tokens) => tokens,
        Err(payload) => match payload.downcast::<Abort>() {
            Ok(abort) => abort.0.to_compile_error().into(),
            Err(payload) => resume_unwind(payload),
        },
    }
}

/// An error at a span (`abort!(span, ..)`).
#[allow(dead_code)]
pub(crate) trait SpanError {
    fn abort_error(&self, message: String) -> syn::Error;
}

impl SpanError for Span {
    fn abort_error(&self, message: String) -> syn::Error {
        syn::Error::new(*self, message)
    }
}

/// An error over the tokens of a syntax node (`abort!(node, ..)`).
#[allow(dead_code)]
pub(crate) trait TokensError {
    fn abort_error(&self, message: String) -> syn::Error;
}

impl<T: ToTokens> TokensError for T {
    fn abort_error(&self, message: String) -> syn::Error {
        syn::Error::new_spanned(self, message)
    }
}

macro_rules! abort {
    // As proc-macro-error rendered notes on stable Rust: appended to the
    // message (the note's own span is not shown).
    ($at:expr, $msg:literal $(, $args:expr)* ; note = $note_at:expr => $note:literal $(,)?) => {{
        #[allow(unused_imports)]
        use $crate::abort::{SpanError as _, TokensError as _};
        let _ = &$note_at;
        let message = ::std::format!($msg $(, $args)*);
        $crate::abort::abort_with(($at).abort_error(::std::format!(
            "{}\n\n  = note: {}\n\n",
            message.trim_end_matches('\n'),
            ::std::string::ToString::to_string(&$note).trim_end_matches('\n'),
        )))
    }};
    ($at:expr, $msg:literal $(, $args:expr)* $(,)?) => {{
        #[allow(unused_imports)]
        use $crate::abort::{SpanError as _, TokensError as _};
        $crate::abort::abort_with(($at).abort_error(::std::format!($msg $(, $args)*)))
    }};
    ($at:expr, $msg:expr $(,)?) => {{
        #[allow(unused_imports)]
        use $crate::abort::{SpanError as _, TokensError as _};
        $crate::abort::abort_with(($at).abort_error(::std::string::ToString::to_string(&$msg)))
    }};
}

macro_rules! abort_call_site {
    ($msg:literal $(, $args:expr)* $(,)?) => {
        $crate::abort::abort_with(::syn::Error::new(
            ::proc_macro2::Span::call_site(),
            ::std::format!($msg $(, $args)*),
        ))
    };
    ($msg:expr $(,)?) => {
        $crate::abort::abort_with(::syn::Error::new(
            ::proc_macro2::Span::call_site(),
            ::std::string::ToString::to_string(&$msg),
        ))
    };
}

#[allow(unused_imports)]
pub(crate) use {abort, abort_call_site};
