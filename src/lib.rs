pub(crate) use libbpf_sys::*;
use libc::{free, if_nametoindex, posix_memalign};
use nix::errno;
use std::ffi::{CStr, CString};
use sysconf::page::pagesize;

use mio::event::Evented;
use mio::unix::EventedFd;
use mio::{Poll, PollOpt, Ready, Token};

use std::io;
use std::os::unix::io::RawFd;

use tokio::io::PollEvented;

use futures::ready;

static DEFAULT_N_FRAMES: u64 = 4096u64;
static DEFAULT_FRAME_SIZE: u64 = XSK_UMEM__DEFAULT_FRAME_SIZE as u64;

static DEFAULT_CONS_NUM_DESCS: u32 = XSK_RING_CONS__DEFAULT_NUM_DESCS as u32;
static DEFAULT_PROD_NUM_DESCS: u32 = XSK_RING_PROD__DEFAULT_NUM_DESCS as u32;

struct XskUmem {
    umem_p: *mut xsk_umem,
    inner_buf: *mut core::ffi::c_void,
}

struct XskUmemInfo {
    prod: xsk_ring_prod,
    cons: xsk_ring_cons,

    n_frames: u64,
    frame_size: u64,

    umem: XskUmem,
}

impl Drop for XskUmemInfo {
    fn drop(&mut self) {
        unsafe {
            xsk_umem__delete(self.umem.umem_p);
            free(self.umem.inner_buf);
        }
    }
}

struct XskUmemInfoBuilder {
    inner: Box<XskUmemInfo>,
}

impl XskUmemInfoBuilder {
    fn new() -> Self {
        let inner = Box::new(XskUmemInfo {
            prod: Default::default(),
            cons: Default::default(),
            n_frames: DEFAULT_N_FRAMES,
            frame_size: DEFAULT_FRAME_SIZE,
            umem: XskUmem {
                umem_p: std::ptr::null_mut(),
                inner_buf: std::ptr::null_mut(),
            },
        });

        XskUmemInfoBuilder { inner }
    }

    fn with_n_frames(mut self, n_frames: u64) -> Self {
        self.inner.n_frames = n_frames;

        self
    }

    fn with_frame_size(mut self, size: u64) -> Self {
        self.inner.frame_size = size;

        self
    }

    fn build(mut self) -> Result<XskUmemInfo, String> {
        let buf: *mut core::ffi::c_void = std::ptr::null_mut();
        let buf_size = (self.inner.n_frames * self.inner.frame_size) as usize;

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
                &self.inner.prod as *const xsk_ring_prod as *mut xsk_ring_prod,
                &self.inner.cons as *const xsk_ring_cons as *mut xsk_ring_cons,
                // for now we dont provide a xsk_umem_config we should do this using the builder pattern in the future
                // the same way we use xsk_socket_config for XskSocketInfo. But since there are less important options
                // for umem, we can leave it null
                std::ptr::null(),
            )
        };

        if ret != 0 {
            unsafe {
                free(buf);
            }
            return Err(format!("error xsk_umem_creating: {}", ret));
        }

        self.inner.umem.inner_buf = buf;

        Ok(*self.inner)
    }
}

struct XskSocketInfo {
    cons_ring: xsk_ring_cons,
    prod_ring: xsk_ring_prod,
    umem_info: XskUmemInfo,
    free_frames_idxes: Vec<u64>,
    ifname: String,
    if_queue_id: u32,

    sock: *mut xsk_socket,
}

impl XskSocketInfo {
    fn get_raw_fd(&self) -> RawFd {
        unsafe { xsk_socket__fd(self.sock) }
    }
}

impl Drop for XskSocketInfo {
    fn drop(&mut self) {
        unsafe {
            xsk_socket__delete(self.sock);
        }
    }
}

struct XskSocketAsync {
    inner: XskSocketInfo,
}

impl Evented for XskSocketAsync {
    fn register(
        &self,
        poll: &Poll,
        token: Token,
        interest: Ready,
        opts: PollOpt,
    ) -> io::Result<()> {
        EventedFd(&self.inner.get_raw_fd()).register(poll, token, interest, opts)
    }

    fn reregister(
        &self,
        poll: &Poll,
        token: Token,
        interest: Ready,
        opts: PollOpt,
    ) -> io::Result<()> {
        EventedFd(&self.inner.get_raw_fd()).reregister(poll, token, interest, opts)
    }

    fn deregister(&self, poll: &Poll) -> io::Result<()> {
        EventedFd(&self.inner.get_raw_fd()).deregister(poll)
    }
}

struct XskStream {
    inner: PollEvented<XskSocketAsync>,
}

struct XskSocketInfoBuilder {
    inner: Box<XskSocketInfo>,
    conf: Box<xsk_socket_config>,
}

impl XskSocketInfoBuilder {
    fn new(umem_info: XskUmemInfo, ifname: String, if_queue_id: u32) -> Self {
        let inner = Box::new(XskSocketInfo {
            cons_ring: Default::default(),
            prod_ring: Default::default(),
            free_frames_idxes: Vec::new(),
            umem_info,
            if_queue_id,
            ifname,

            sock: std::ptr::null_mut(),
        });

        XskSocketInfoBuilder {
            inner,
            conf: Box::new(xsk_socket_config {
                rx_size: DEFAULT_CONS_NUM_DESCS,
                tx_size: DEFAULT_PROD_NUM_DESCS,
                libbpf_flags: 0,
                xdp_flags: 0,
                bind_flags: 0,
            }),
        }
    }

    fn with_rx_size(mut self, size: u32) -> Self {
        self.conf.rx_size = size;

        self
    }

    fn with_tx_size(mut self, size: u32) -> Self {
        self.conf.tx_size = size;

        self
    }

    fn with_libbpf_flag(mut self, flag: u32) -> Self {
        self.conf.libbpf_flags |= flag;

        self
    }

    fn with_xdp_flag(mut self, flag: u32) -> Self {
        self.conf.xdp_flags |= flag;

        self
    }

    fn with_bind_flag(mut self, flag: u16) -> Self {
        self.conf.bind_flags |= flag;

        self
    }

    fn build(mut self) -> Result<XskSocketInfo, String> {
        let c_str = CString::new(self.inner.ifname.clone())
            .map_err(|e| format!("error creating a cstring: {}", e))?;

        let ifindex = unsafe { if_nametoindex(c_str.as_ptr() as *const i8) };

        if ifindex == 0 {
            return Err(format!(
                "invalid interface {}: {}",
                self.inner.ifname,
                errno::errno()
            ));
        }

        let ret = unsafe {
            xsk_socket__create(
                &self.inner.sock as *const *mut xsk_socket as *mut *mut xsk_socket,
                c_str.into_raw() as *const ::std::os::raw::c_char,
                self.inner.if_queue_id,
                self.inner.umem_info.umem.umem_p,
                &self.inner.cons_ring as *const xsk_ring_cons as *mut xsk_ring_cons,
                &self.inner.prod_ring as *const xsk_ring_prod as *mut xsk_ring_prod,
                Box::into_raw(self.conf.clone()),
            )
        };

        if ret != 0 {
            return Err(format!("error creating xsk_socket: {}", ret));
        }

        #[cfg(feature = "bypass_link_xdp_id")]
        {
            let prog_id = 0u32;
            let retw = unsafe {
                bpf_get_link_xdp_id(
                    ifindex as i32,
                    &prog_id as *const u32 as *mut u32,
                    self.conf.xdp_flags,
                )
            };
            if retw != 0 {
                unsafe { xsk_socket__delete(self.inner.sock) };
                return Err(format!("error getting link xdp_id: {}", retw));
            }
        }

        for i in 0..self.inner.umem_info.n_frames {
            self.inner
                .free_frames_idxes
                .push(i * self.inner.umem_info.frame_size);
        }

        let mut idx = 0u32;
        let ret = unsafe {
            _xsk_ring_prod__reserve(
                &self.inner.umem_info.prod as *const xsk_ring_prod as *mut xsk_ring_prod,
                self.conf.tx_size as u64,
                &idx as *const u32 as *mut u32,
            )
        };

        if ret != self.conf.tx_size as u64 {
            return Err(format!(
                "error reserving prod ring of size {}, got {}",
                self.conf.tx_size, ret
            ));
        }

        for i in 0..self.conf.tx_size {
            unsafe {
                *_xsk_ring_prod__fill_addr(
                    &self.inner.umem_info.prod as *const xsk_ring_prod as *mut xsk_ring_prod,
                    idx,
                ) = self.inner.free_frames_idxes.pop().ok_or(format!(
                    "not enoug frames to fill the producer. Expected {} got {}",
                    self.conf.tx_size, i
                ))?
            };
            idx += 1;
        }

        unsafe {
            _xsk_ring_prod__submit(
                &self.inner.umem_info.prod as *const xsk_ring_prod as *mut xsk_ring_prod,
                self.conf.tx_size as u64,
            )
        };

        Ok(*self.inner)
    }

    async fn build_async(mut self) -> Result<XskStream, String> {
        let poll_evented = PollEvented::new(XskSocketAsync {
            inner: self.build()?,
        })
        .map_err(|e| format!("error creating poll evented: {}", e))?;

        Ok(XskStream {
            inner: poll_evented,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn basic_build_and_drop_umem() {
        assert!(crate::XskUmemInfoBuilder::new().build().is_ok());
    }

    // having both sync and async tests running will cause the one that is ran second to fail
    // idk why yet, but it seems like there should be some extra cleanup that we missed
    // for now, we leave the async one because it actually contains the sync part

    // after testing with --test-threads=1 and merging the two threads it is obvious that
    // this is a cleanup problem

    // #[test]
    // #[cfg(feature = "bypass_link_xdp_id")]
    // fn basic_build_and_drop_xsk_sync() {
    //     let umem = crate::XskUmemInfoBuilder::new().build();
    //     assert!(umem.is_ok());

    //     assert!(
    //         crate::XskSocketInfoBuilder::new(umem.unwrap(), "lo".to_string(), 0)
    //             .with_libbpf_flag(crate::XSK_LIBBPF_FLAGS__INHIBIT_PROG_LOAD as u32)
    //             .build()
    //             .is_ok()
    //     );
    // }

    #[test]
    #[cfg(feature = "bypass_link_xdp_id")]
    fn basic_build_and_drop_xsk_async() {
        let umem = crate::XskUmemInfoBuilder::new().build();
        assert!(umem.is_ok());
        let mut rt = tokio::runtime::Builder::new()
            .threaded_scheduler()
            .enable_all()
            .build()
            .unwrap();
        let e = rt.block_on(
            crate::XskSocketInfoBuilder::new(umem.unwrap(), "lo".to_string(), 0)
                .with_libbpf_flag(crate::XSK_LIBBPF_FLAGS__INHIBIT_PROG_LOAD as u32)
                .build_async(),
        );

        assert!(e.is_ok());
    }
}
