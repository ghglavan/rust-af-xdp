pub(crate) use libbpf_sys::*;
use libc::{free, if_nametoindex, posix_memalign};
use sysconf::page::pagesize;

use crate::defaults::*;

pub type Frame = u64;

pub(crate) struct XskUmem {
    pub(crate) umem_p: *mut xsk_umem,
    pub(crate) inner_buf: *mut core::ffi::c_void,
}

pub struct XskUmemInfo {
    pub(crate) fill_ring: xsk_ring_prod,
    pub(crate) comp_ring: xsk_ring_cons,

    pub(crate) fill_size: u32,
    pub(crate) comp_size: u32,

    pub(crate) frames: Vec<Frame>,

    pub(crate) n_frames: u64,
    pub(crate) frame_size: u32,

    pub(crate) umem: XskUmem,
}

impl Drop for XskUmemInfo {
    fn drop(&mut self) {
        unsafe {
            xsk_umem__delete(self.umem.umem_p);
            free(self.umem.inner_buf);
        }
    }
}

pub struct XskUmemInfoBuilder {
    inner: Box<XskUmemInfo>,
    conf: Box<xsk_umem_config>,
}

impl XskUmemInfoBuilder {
    pub fn new() -> Self {
        let inner = Box::new(XskUmemInfo {
            fill_ring: Default::default(),
            fill_size: DEFAULT_PROD_NUM_DESCS,
            comp_size: DEFAULT_CONS_NUM_DESCS,
            comp_ring: Default::default(),
            n_frames: DEFAULT_N_FRAMES,
            frame_size: DEFAULT_FRAME_SIZE,
            frames: Vec::new(),
            umem: XskUmem {
                umem_p: std::ptr::null_mut(),
                inner_buf: std::ptr::null_mut(),
            },
        });

        let conf = Box::new(xsk_umem_config {
            fill_size: DEFAULT_PROD_NUM_DESCS,
            comp_size: DEFAULT_CONS_NUM_DESCS,
            frame_size: DEFAULT_FRAME_SIZE,
            frame_headroom: DEFAULT_HEADROOM,
            flags: DEFAULT_FLAGS,
        });

        XskUmemInfoBuilder { inner, conf }
    }

    pub fn with_fill_size(mut self, size: u32) -> Result<Self, String> {
        if size & size - 1 != 0 {
            return Err("fill size should be a multiple of 2".to_string());
        }

        self.conf.fill_size = size;
        self.inner.fill_size = size;

        Ok(self)
    }

    pub fn with_comp_size(mut self, size: u32) -> Result<Self, String> {
        if size & size - 1 != 0 {
            return Err("comp size should be a multiple of 2".to_string());
        }

        self.conf.comp_size = size;
        self.inner.comp_size = size;

        Ok(self)
    }

    pub fn with_n_frames(mut self, n_frames: u64) -> Self {
        self.inner.n_frames = n_frames;

        self
    }

    pub fn with_frame_size(mut self, size: u32) -> Result<Self, String> {
        if size & size - 1 != 0 {
            return Err("comp size should be a multiple of 2".to_string());
        }

        self.inner.frame_size = size;
        self.conf.frame_size = size;

        Ok(self)
    }

    pub fn build(mut self) -> Result<XskUmemInfo, String> {
        let buf: *mut core::ffi::c_void = std::ptr::null_mut();
        let buf_size = (self.inner.n_frames * self.inner.frame_size as u64) as usize;

        // todo: we should probably provide more options here
        let ret = unsafe {
            libc::posix_memalign(
                &buf as *const *mut std::ffi::c_void as *mut *mut std::ffi::c_void,
                pagesize(),
                buf_size,
            )
        };

        if ret != 0 {
            return Err(format!("error posix memaligning: {}", ret));
        }

        let ret = unsafe {
            xsk_umem__create(
                &self.inner.umem.umem_p as *const *mut xsk_umem as *mut *mut xsk_umem,
                buf,
                buf_size as u64,
                &self.inner.fill_ring as *const xsk_ring_prod as *mut xsk_ring_prod,
                &self.inner.comp_ring as *const xsk_ring_cons as *mut xsk_ring_cons,
                Box::into_raw(self.conf.clone()),
            )
        };

        if ret != 0 {
            unsafe {
                free(buf);
            }
            return Err(format!("error xsk_umem_creating: {}", ret));
        }

        self.inner.umem.inner_buf = buf;

        for i in 0..self.inner.n_frames {
            self.inner.frames.push(i * self.inner.frame_size as u64);
        }

        Ok(*self.inner)
    }
}
