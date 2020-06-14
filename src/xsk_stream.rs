use crate::defaults::*;
use crate::umem;

pub(crate) use libbpf_sys::*;

use mio::event::Evented;
use mio::unix::EventedFd;
use mio::{PollOpt, Ready, Token};

use std::collections::VecDeque;
use std::io;
use std::os::unix::io::RawFd;
use std::task::Poll;
use std::time::{Duration, Instant};

use nix::errno;
use std::ffi::CString;

use libc::if_nametoindex;

use tokio::io::{AsyncRead, AsyncWrite, PollEvented};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{channel, Receiver, Sender};

use futures::task::Context;
use std::sync::{Arc, Mutex};

use futures::stream::Stream;

use core::pin::Pin;

pub struct XskSocketInfo {
    rx_ring: xsk_ring_cons,
    tx_ring: xsk_ring_prod,

    rx_size: u32,
    tx_size: u32,

    // TODO:
    // fow now we have a single socket for a
    // umem. in the future we can change that
    // but this means that we either do some
    // futures magic to synchronize the rings
    // or we just Arc<Mutex<UMEM>>
    umem_info: umem::XskUmemInfo,
    rx_batch_size: u32,
    tx_batch_size: u32,

    ifname: String,
    if_queue_id: u32,

    sock: *mut xsk_socket,
}

impl XskSocketInfo {
    fn get_raw_fd(&self) -> RawFd {
        unsafe { xsk_socket__fd(self.sock) }
    }

    fn reserve_frames_for_tx_ring(&mut self, n_frames: u32) -> Result<(), String> {
        if n_frames > self.tx_size {
            return Err(format!(
                "Not enough space in fill_ring: max {} got {}",
                self.tx_size, n_frames
            ));
        }

        if n_frames > self.umem_info.frames.len() as u32 {
            return Err(format!(
                "Not enough frames: max {} got {}",
                self.umem_info.frames.len(),
                n_frames
            ));
        }

        let mut idx = 0u32;
        let ret = unsafe {
            _xsk_ring_prod__reserve(
                &self.tx_ring as *const xsk_ring_prod as *mut xsk_ring_prod,
                n_frames as u64,
                &idx as *const u32 as *mut u32,
            )
        };

        if ret != n_frames as u64 {
            return Err(format!(
                "error reserving tx ring of size {}, got {}",
                n_frames, ret
            ));
        }

        for i in 0..n_frames {
            unsafe {
                *_xsk_ring_prod__fill_addr(
                    &self.tx_ring as *const xsk_ring_prod as *mut xsk_ring_prod,
                    idx,
                ) = self.umem_info.frames.pop().ok_or(format!(
                    "not enoug frames to fill the producer. Expected {} got {}",
                    n_frames, i
                ))?
            };
            idx += 1;
        }

        unsafe {
            _xsk_ring_prod__submit(
                &self.tx_ring as *const xsk_ring_prod as *mut xsk_ring_prod,
                n_frames as u64,
            )
        };

        Ok(())
    }

    fn reserve_frames_for_fill_ring(&mut self, n_frames: u32) -> Result<(), String> {
        if n_frames > self.umem_info.fill_size {
            return Err(format!(
                "Not enough space in fill_ring: max {} got {}",
                self.umem_info.fill_size, n_frames
            ));
        }

        if n_frames > self.umem_info.frames.len() as u32 {
            return Err(format!(
                "Not enough frames: max {} got {}",
                self.umem_info.frames.len(),
                n_frames
            ));
        }

        let mut idx = 0u32;
        let ret = unsafe {
            _xsk_ring_prod__reserve(
                &self.umem_info.fill_ring as *const xsk_ring_prod as *mut xsk_ring_prod,
                n_frames as u64,
                &idx as *const u32 as *mut u32,
            )
        };

        if ret != n_frames as u64 {
            return Err(format!(
                "error reserving prod ring of size {}, got {}",
                n_frames, ret
            ));
        }

        for i in 0..n_frames {
            unsafe {
                *_xsk_ring_prod__fill_addr(
                    &self.umem_info.fill_ring as *const xsk_ring_prod as *mut xsk_ring_prod,
                    idx,
                ) = self.umem_info.frames.pop().ok_or(format!(
                    "not enoug frames to fill the producer. Expected {} got {}",
                    n_frames, i
                ))?
            };
            idx += 1;
        }

        unsafe {
            _xsk_ring_prod__submit(
                &self.umem_info.fill_ring as *const xsk_ring_prod as *mut xsk_ring_prod,
                n_frames as u64,
            )
        };
        Ok(())
    }
}

impl Drop for XskSocketInfo {
    fn drop(&mut self) {
        unsafe {
            xsk_socket__delete(self.sock);
        }
    }
}

pub struct XskSocketInfoBuilder {
    inner: Box<XskSocketInfo>,
    conf: Box<xsk_socket_config>,

    write_timeout: Option<Duration>,
}

impl XskSocketInfoBuilder {
    pub fn new(umem_info: umem::XskUmemInfo, ifname: String, if_queue_id: u32) -> Self {
        let inner = Box::new(XskSocketInfo {
            rx_ring: Default::default(),
            tx_ring: Default::default(),
            rx_size: DEFAULT_CONS_NUM_DESCS,
            tx_size: DEFAULT_PROD_NUM_DESCS,
            rx_batch_size: DEFAULT_BATCH_SIZE,
            tx_batch_size: DEFAULT_BATCH_SIZE,
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
            write_timeout: None,
        }
    }

    fn with_rx_size(mut self, size: u32) -> Self {
        self.conf.rx_size = size;
        self.inner.rx_size = size;

        self
    }

    pub fn with_rx_batch_size(mut self, size: u32) -> Self {
        self.inner.rx_batch_size = size;

        self
    }

    fn with_tx_batch_size(mut self, size: u32) -> Self {
        self.inner.tx_batch_size = size;

        self
    }

    fn with_tx_size(mut self, size: u32) -> Self {
        self.conf.tx_size = size;
        self.inner.tx_size = size;

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

    fn with_write_timeout(mut self, d: Duration) -> Self {
        self.write_timeout = Some(d);

        self
    }

    pub fn build(mut self) -> Result<XskSocketInfo, String> {
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
                &self.inner.rx_ring as *const xsk_ring_cons as *mut xsk_ring_cons,
                &self.inner.tx_ring as *const xsk_ring_prod as *mut xsk_ring_prod,
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

        Ok(*self.inner)
    }

    pub async fn build_async(self) -> Result<XskStream, String> {
        let rx_batch_size = self.inner.rx_batch_size;
        let tx_batch_size = self.inner.tx_batch_size;
        let write_timeout = self.write_timeout;
        let poll_evented = PollEvented::new(XskSocketAsync {
            inner: self.build()?,
        })
        .map_err(|e| format!("error creating poll evented: {}", e))?;

        Ok(XskStream {
            inner: poll_evented,
            last_write: Instant::now(),
            write_timeout,
            read_frames: 0,
            buf_read: VecDeque::with_capacity(rx_batch_size as usize),
            buf_write: VecDeque::with_capacity(tx_batch_size as usize),
        })
    }
}

struct XskSocketAsync {
    inner: XskSocketInfo,
}

impl Evented for XskSocketAsync {
    fn register(
        &self,
        poll: &mio::Poll,
        token: Token,
        interest: Ready,
        opts: PollOpt,
    ) -> io::Result<()> {
        EventedFd(&self.inner.get_raw_fd()).register(poll, token, interest, opts)
    }

    fn reregister(
        &self,
        poll: &mio::Poll,
        token: Token,
        interest: Ready,
        opts: PollOpt,
    ) -> io::Result<()> {
        EventedFd(&self.inner.get_raw_fd()).reregister(poll, token, interest, opts)
    }

    fn deregister(&self, poll: &mio::Poll) -> io::Result<()> {
        EventedFd(&self.inner.get_raw_fd()).deregister(poll)
    }
}

pub struct XskPacket<'a> {
    buf: &'a mut [u8],
    orig: u64,
}

impl<'a> XskPacket<'a> {
    fn get_buf(&'a mut self) -> &'a mut [u8] {
        self.buf
    }
}

pub struct XskStream {
    inner: PollEvented<XskSocketAsync>,
    buf_read: VecDeque<xdp_desc>,
    buf_write: VecDeque<xdp_desc>,
    read_frames: u64,

    last_write: Instant,
    write_timeout: Option<Duration>,
}

impl AsyncRead for XskStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        if buf.len() != std::mem::size_of::<xdp_desc>() {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "expected {} bytes as buf for poll_read, got {}",
                    std::mem::size_of::<xdp_desc>(),
                    buf.len()
                ),
            )));
        }

        let cons = self.get_mut();

        if let Some(f) = cons.buf_read.pop_front() {
            // fast path, we have a packet buffered in a desc
            buf.copy_from_slice(unsafe {
                std::slice::from_raw_parts(
                    &f as *const xdp_desc as *const u8,
                    std::mem::size_of::<xdp_desc>(),
                )
            });
            return std::task::Poll::Ready(Ok(std::mem::size_of::<xdp_desc>()));
        }

        // slow path, we need to actually recv something

        match cons.inner.poll_read_ready(cx, mio::Ready::readable()) {
            std::task::Poll::Ready(t) => t,
            std::task::Poll::Pending => {
                return std::task::Poll::Pending;
            }
        }?;

        let mut rx_index = 0u32;

        let n_rcv = unsafe {
            _xsk_ring_cons__peek(
                &cons.inner.get_ref().inner.rx_ring as *const xsk_ring_cons as *mut xsk_ring_cons,
                cons.inner.get_ref().inner.rx_batch_size as u64,
                &rx_index as *const u32 as *mut u32,
            )
        };

        // test if we can give more frames to the fill_queue
        let n_free_frames = cons.inner.get_ref().inner.umem_info.frames.len();

        if cons.buf_write.len() < n_free_frames
            && cons.read_frames - n_rcv < cons.inner.get_ref().inner.rx_batch_size as u64
        {
            // we have enough frames to handle all the writes + some new reads
            let n_fill_frames = n_free_frames - cons.buf_write.len();
            // dont allocate more than a batch
            let n_fill_frames = if n_fill_frames < cons.inner.get_ref().inner.rx_batch_size as usize
            {
                n_fill_frames
            } else {
                cons.inner.get_ref().inner.rx_batch_size as usize
            };

            let to_reserve = unsafe {
                _xsk_prod_nb_free(
                    &cons.inner.get_ref().inner.umem_info.fill_ring as *const xsk_ring_prod
                        as *mut xsk_ring_prod,
                    n_fill_frames as u32,
                )
            };

            let mut idx_fq = 0u32;
            if to_reserve > 0 {
                unsafe {
                    _xsk_ring_prod__reserve(
                        &cons.inner.get_ref().inner.umem_info.fill_ring as *const xsk_ring_prod
                            as *mut xsk_ring_prod,
                        to_reserve as u64,
                        &idx_fq as *const u32 as *mut u32,
                    );
                };

                for i in 0..to_reserve {
                    let f = match cons.inner.get_mut().inner.umem_info.frames.pop() {
                        Some(f) => f,
                        None => {
                            return std::task::Poll::Ready(Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!(
                                    "logical error, expected to pop {} frames, poped only {}",
                                    to_reserve, i
                                ),
                            )))
                        }
                    };
                    unsafe {
                        *_xsk_ring_prod__fill_addr(
                            &cons.inner.get_ref().inner.umem_info.fill_ring as *const xsk_ring_prod
                                as *mut xsk_ring_prod,
                            idx_fq,
                        ) = f;
                    }
                    idx_fq += 1;
                }

                unsafe {
                    _xsk_ring_prod__submit(
                        &cons.inner.get_ref().inner.umem_info.fill_ring as *const xsk_ring_prod
                            as *mut xsk_ring_prod,
                        to_reserve as u64,
                    )
                }

                cons.read_frames += to_reserve as u64;
            }
        }

        if n_rcv == 0 {
            cons.inner.clear_read_ready(cx, mio::Ready::readable())?;
            return std::task::Poll::Pending;
        }

        // take the first frame
        let first = match cons.get_addr_by_idx(rx_index) {
            Some(d) => d,
            None => {
                return std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("error getting addr by index {}", rx_index),
                )))
            }
        };

        cons.read_frames -= n_rcv;

        // buffer the rest
        for _ in 1..n_rcv {
            let desc = match cons.get_addr_by_idx(rx_index) {
                Some(d) => d,
                None => {
                    return std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("error getting addr by index {}", rx_index),
                    )))
                }
            };
            cons.buf_read.push_back(desc);
            rx_index += 1;
        }

        buf.copy_from_slice(unsafe {
            std::slice::from_raw_parts(
                &first as *const xdp_desc as *const u8,
                std::mem::size_of::<xdp_desc>(),
            )
        });
        return std::task::Poll::Ready(Ok(std::mem::size_of::<xdp_desc>()));
    }
}

impl AsyncWrite for XskStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // if buf.len() == 0 {
        //     return std::task::Poll::Ready(Err(std::io::Error::new(
        //         std::io::ErrorKind::InvalidInput,
        //         format!("null buffer"),
        //     )));
        // }

        // let mut prod = self.get_mut();
        // let tx_idx = 0u32;

        // let mut should_write = prod.write_timeout == None;

        // if !should_write {
        //     if prod.last_write.elapsed() > prod.write_timeout.unwrap() {
        //         should_write = true;
        //     } else {
        //         // fast_path buffering writes
        //     }
        // }

        // check if we can actually write anything
        // match prod.inner.poll_write_ready(cx) {
        //     std::task::Poll::Ready(t) => t,

        //     prod.inner.clear_write_ready(cx)?;
        //     std::task::Poll::Pending => return std::task::Poll::Pending,
        // }?;

        // no write batching
        // if should_write {
        //     let ret = unsafe {
        //         _xsk_ring_prod__reserve(
        //             &prod.inner.get_ref().inner.tx_ring as *const xsk_ring_prod
        //                 as *mut xsk_ring_prod,
        //             1,
        //             &tx_idx as *const u32 as *mut u32,
        //         )
        //     };
        // }

        // prod.last_write = Instant::now();

        std::task::Poll::Pending
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), tokio::io::Error>> {
        std::task::Poll::Pending
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), tokio::io::Error>> {
        std::task::Poll::Pending
    }
}

impl XskStream {
    fn get_addr_by_idx(&mut self, idx: u32) -> Option<xdp_desc> {
        let addr = unsafe {
            _xsk_ring_cons__rx_desc(
                &self.inner.get_ref().inner.rx_ring as *const xsk_ring_cons,
                idx,
            )
        };

        if addr == std::ptr::null_mut() {
            return None;
        };

        Some(unsafe { *addr })
    }

    pub fn get_buf_size(&self) -> usize {
        std::mem::size_of::<xdp_desc>()
    }
}

impl<'a> XskStream {
    pub fn get_packet_by_addr_buf(&self, buf: &'a [u8]) -> XskPacket<'a> {
        let addr = buf.as_ptr() as *const xdp_desc;
        let addr = unsafe { *addr };
        let (addr, len) = (addr.addr, addr.len);
        let orig = addr;

        let addr = unsafe { _xsk_umem__add_offset_to_addr(addr) };
        let buf = unsafe {
            _xsk_umem__get_data(self.inner.get_ref().inner.umem_info.umem.inner_buf, addr)
        };

        XskPacket {
            buf: unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, len as usize) },
            orig,
        }
    }

    pub fn clean_packet(&'a mut self, packet: XskPacket) {
        self.inner
            .get_mut()
            .inner
            .umem_info
            .frames
            .push(packet.orig);

        unsafe {
            _xsk_ring_cons__release(
                &self.inner.get_ref().inner.rx_ring as *const xsk_ring_cons as *mut xsk_ring_cons,
                1,
            );
        }
    }

    pub fn reserve_frames_for_fill_ring(&mut self, nr_frames: u32) -> Result<(), String> {
        let r = self
            .inner
            .get_mut()
            .inner
            .reserve_frames_for_fill_ring(nr_frames);

        if r.is_ok() {
            self.read_frames += nr_frames as u64;
        }

        r
    }
}
