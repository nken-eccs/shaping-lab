//! HarfBuzzの公開C ABI
#![allow(non_camel_case_types)]
use std::ffi::{c_char, c_int, c_uint, c_void};

pub type Handle = c_void;
pub type Language = *const c_void;

#[repr(C)]
pub struct Feature {
    pub tag: u32,
    pub value: u32,
    pub start: u32,
    pub end: u32,
}
#[repr(C)]
pub struct Variation {
    pub tag: u32,
    pub value: f32,
}
#[repr(C)]
pub struct Info {
    pub codepoint: u32,
    pub mask: u32,
    pub cluster: u32,
    pub var1: u32,
    pub var2: u32,
}
#[repr(C)]
pub struct Position {
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
    pub var: u32,
}
#[repr(C)]
pub struct Properties {
    pub direction: u32,
    pub script: u32,
    pub language: Language,
    pub reserved1: *mut c_void,
    pub reserved2: *mut c_void,
}
pub type MessageFn =
    unsafe extern "C" fn(*mut Handle, *mut Handle, *const c_char, *mut c_void) -> c_int;

unsafe extern "C" {
    pub fn shaping_lab_harfbuzz_assertions_enabled() -> c_int;
    pub fn hb_version_string() -> *const c_char;
    pub fn hb_blob_create(
        data: *const c_char,
        length: c_uint,
        mode: u32,
        user_data: *mut c_void,
        destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> *mut Handle;
    pub fn hb_blob_destroy(blob: *mut Handle);
    pub fn hb_face_count(blob: *mut Handle) -> c_uint;
    pub fn hb_face_create(blob: *mut Handle, index: c_uint) -> *mut Handle;
    pub fn hb_face_destroy(face: *mut Handle);
    pub fn hb_face_get_upem(face: *mut Handle) -> c_uint;
    pub fn hb_face_get_glyph_count(face: *mut Handle) -> c_uint;
    pub fn hb_font_create(face: *mut Handle) -> *mut Handle;
    pub fn hb_font_destroy(font: *mut Handle);
    pub fn hb_font_get_face(font: *mut Handle) -> *mut Handle;
    pub fn hb_ot_font_set_funcs(font: *mut Handle);
    pub fn hb_font_set_scale(font: *mut Handle, x: c_int, y: c_int);
    pub fn hb_font_set_variations(font: *mut Handle, variations: *const Variation, count: c_uint);
    pub fn hb_font_get_var_coords_normalized(
        font: *mut Handle,
        length: *mut c_uint,
    ) -> *const c_int;
    pub fn hb_buffer_create() -> *mut Handle;
    pub fn hb_buffer_destroy(buffer: *mut Handle);
    pub fn hb_buffer_reset(buffer: *mut Handle);
    pub fn hb_buffer_set_segment_properties(buffer: *mut Handle, props: *const Properties);
    pub fn hb_buffer_set_cluster_level(buffer: *mut Handle, level: u32);
    pub fn hb_buffer_set_flags(buffer: *mut Handle, flags: u32);
    pub fn hb_buffer_add_utf8(
        buffer: *mut Handle,
        text: *const c_char,
        text_length: c_int,
        offset: c_uint,
        length: c_int,
    );
    pub fn hb_buffer_get_glyph_infos(buffer: *mut Handle, length: *mut c_uint) -> *mut Info;
    pub fn hb_buffer_get_glyph_positions(buffer: *mut Handle, length: *mut c_uint)
    -> *mut Position;
    pub fn hb_buffer_get_length(buffer: *mut Handle) -> c_uint;
    pub fn hb_buffer_allocation_successful(buffer: *mut Handle) -> c_int;
    pub fn hb_buffer_set_message_func(
        buffer: *mut Handle,
        func: Option<MessageFn>,
        data: *mut c_void,
        destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    );
    pub fn hb_language_from_string(text: *const c_char, length: c_int) -> Language;
    pub fn hb_script_from_string(text: *const c_char, length: c_int) -> u32;
    pub fn hb_shape_full(
        font: *mut Handle,
        buffer: *mut Handle,
        features: *const Feature,
        count: c_uint,
        shapers: *const *const c_char,
    ) -> c_int;
    pub fn hb_shape_plan_create2(
        face: *mut Handle,
        props: *const Properties,
        features: *const Feature,
        count: c_uint,
        coords: *const c_int,
        coord_count: c_uint,
        shapers: *const *const c_char,
    ) -> *mut Handle;
    pub fn hb_shape_plan_execute(
        plan: *mut Handle,
        font: *mut Handle,
        buffer: *mut Handle,
        features: *const Feature,
        count: c_uint,
    ) -> c_int;
    pub fn hb_shape_plan_destroy(plan: *mut Handle);
}
