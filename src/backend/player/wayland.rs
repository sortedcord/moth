//! Embedding waylandsink's output in the app window.

use std::ffi::c_void;
use std::ptr::NonNull;

use gstreamer as gst;
use gstreamer::glib::translate::from_glib_full;

#[link(name = "gstwayland-1.0")]
unsafe extern "C" {
    fn gst_wl_display_handle_context_new(display: *mut c_void) -> *mut gst::ffi::GstContext;
}

/// The window's `wl_display` and `wl_surface`. waylandsink shares the connection and attaches a
/// subsurface to the surface.
#[derive(Clone, Copy, Debug)]
pub struct WaylandTarget {
    display: NonNull<c_void>,
    surface: NonNull<c_void>,
}

impl WaylandTarget {
    /// # Safety
    /// Both pointers must stay valid for as long as any player uses this target (i.e. the
    /// window must outlive its players).
    pub unsafe fn new(display: NonNull<c_void>, surface: NonNull<c_void>) -> Self {
        Self { display, surface }
    }

    pub(super) fn display_context(&self) -> gst::Context {
        // SAFETY: the display pointer is valid (see `new`); the function returns a new reference.
        unsafe { from_glib_full(gst_wl_display_handle_context_new(self.display.as_ptr())) }
    }

    pub(super) fn surface_handle(&self) -> usize {
        self.surface.as_ptr() as usize
    }
}
