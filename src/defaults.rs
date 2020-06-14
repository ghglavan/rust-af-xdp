pub(crate) use libbpf_sys::*;

pub(crate) static DEFAULT_N_FRAMES: u64 = 4096u64;
pub(crate) static DEFAULT_FRAME_SIZE: u32 = XSK_UMEM__DEFAULT_FRAME_SIZE;
pub(crate) static DEFAULT_HEADROOM: u32 = XSK_UMEM__DEFAULT_FRAME_HEADROOM;
pub(crate) static DEFAULT_FLAGS: u32 = XSK_UMEM__DEFAULT_FLAGS;

pub(crate) static DEFAULT_BATCH_SIZE: u32 = 64;

pub(crate) static DEFAULT_CONS_NUM_DESCS: u32 = XSK_RING_CONS__DEFAULT_NUM_DESCS as u32;
pub(crate) static DEFAULT_PROD_NUM_DESCS: u32 = XSK_RING_PROD__DEFAULT_NUM_DESCS as u32;
