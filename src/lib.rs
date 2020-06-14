pub(crate) mod defaults;

pub mod umem;
pub mod xsk_stream;

// pub(crate) use libbpf_sys::*;
// use libc::{free, if_nametoindex, posix_memalign};
// use nix::errno;
// use std::ffi::{CStr, CString};
// use sysconf::page::pagesize;

// use mio::event::Evented;
// use mio::unix::EventedFd;
// use mio::{PollOpt, Ready, Token};

// use std::io;
// use std::os::unix::io::RawFd;

// use tokio::io::PollEvented;
// use tokio::sync::mpsc::error::TrySendError;
// use tokio::sync::mpsc::{channel, Receiver, Sender};

// use futures::task::Context;
// use std::sync::{Arc, Mutex};

// use futures::stream::Stream;

// use core::pin::Pin;

// struct XskSocketInfo {
//     cons_ring: xsk_ring_cons,
//     prod_ring: xsk_ring_prod,

//     // TODO:
//     // fow now we have a single socket for a
//     // umem. in the future we can change that
//     // but this means that we either do some
//     // futures magic to synchronize the rings
//     // or we just Arc<Mutex<UMEM>>
//     umem_info: XskUmemInfo,
//     free_frame_addrs: Vec<u64>,

//     rx_batch_size: u32,

//     ifname: String,
//     if_queue_id: u32,

//     sock: *mut xsk_socket,
// }

// impl XskSocketInfo {
//     fn get_raw_fd(&self) -> RawFd {
//         unsafe { xsk_socket__fd(self.sock) }
//     }
// }

// impl Drop for XskSocketInfo {
//     fn drop(&mut self) {
//         unsafe {
//             xsk_socket__delete(self.sock);
//         }
//     }
// }

// struct XskSocketInfoBuilder {
//     inner: Box<XskSocketInfo>,
//     conf: Box<xsk_socket_config>,
// }

// impl XskSocketInfoBuilder {
//     fn new(umem_info: XskUmemInfo, ifname: String, if_queue_id: u32) -> Self {
//         let inner = Box::new(XskSocketInfo {
//             cons_ring: Default::default(),
//             prod_ring: Default::default(),
//             free_frame_addrs: Vec::new(),
//             rx_batch_size: DEFAULT_BATCH_SIZE,
//             umem_info,
//             if_queue_id,
//             ifname,

//             sock: std::ptr::null_mut(),
//         });

//         XskSocketInfoBuilder {
//             inner,
//             conf: Box::new(xsk_socket_config {
//                 rx_size: DEFAULT_CONS_NUM_DESCS,
//                 tx_size: DEFAULT_PROD_NUM_DESCS,
//                 libbpf_flags: 0,
//                 xdp_flags: 0,
//                 bind_flags: 0,
//             }),
//         }
//     }

//     fn with_rx_size(mut self, size: u32) -> Self {
//         self.conf.rx_size = size;

//         self
//     }

//     fn with_rx_batch_size(mut self, size: u32) -> Self {
//         self.inner.rx_batch_size = size;

//         self
//     }

//     fn with_tx_size(mut self, size: u32) -> Self {
//         self.conf.tx_size = size;

//         self
//     }

//     fn with_libbpf_flag(mut self, flag: u32) -> Self {
//         self.conf.libbpf_flags |= flag;

//         self
//     }

//     fn with_xdp_flag(mut self, flag: u32) -> Self {
//         self.conf.xdp_flags |= flag;

//         self
//     }

//     fn with_bind_flag(mut self, flag: u16) -> Self {
//         self.conf.bind_flags |= flag;

//         self
//     }

//     fn build(mut self) -> Result<XskSocketInfo, String> {
//         let c_str = CString::new(self.inner.ifname.clone())
//             .map_err(|e| format!("error creating a cstring: {}", e))?;

//         let ifindex = unsafe { if_nametoindex(c_str.as_ptr() as *const i8) };

//         if ifindex == 0 {
//             return Err(format!(
//                 "invalid interface {}: {}",
//                 self.inner.ifname,
//                 errno::errno()
//             ));
//         }

//         let ret = unsafe {
//             xsk_socket__create(
//                 &self.inner.sock as *const *mut xsk_socket as *mut *mut xsk_socket,
//                 c_str.into_raw() as *const ::std::os::raw::c_char,
//                 self.inner.if_queue_id,
//                 self.inner.umem_info.umem.umem_p,
//                 &self.inner.cons_ring as *const xsk_ring_cons as *mut xsk_ring_cons,
//                 &self.inner.prod_ring as *const xsk_ring_prod as *mut xsk_ring_prod,
//                 Box::into_raw(self.conf.clone()),
//             )
//         };

//         if ret != 0 {
//             return Err(format!("error creating xsk_socket: {}", ret));
//         }

//         #[cfg(feature = "bypass_link_xdp_id")]
//         {
//             let prog_id = 0u32;
//             let retw = unsafe {
//                 bpf_get_link_xdp_id(
//                     ifindex as i32,
//                     &prog_id as *const u32 as *mut u32,
//                     self.conf.xdp_flags,
//                 )
//             };
//             if retw != 0 {
//                 unsafe { xsk_socket__delete(self.inner.sock) };
//                 return Err(format!("error getting link xdp_id: {}", retw));
//             }
//         }

//         for i in 0..self.inner.umem_info.n_frames {
//             self.inner
//                 .free_frame_addrs
//                 .push(i * self.inner.umem_info.frame_size);
//         }

//         let mut idx = 0u32;
//         let ret = unsafe {
//             _xsk_ring_prod__reserve(
//                 &self.inner.umem_info.prod as *const xsk_ring_prod as *mut xsk_ring_prod,
//                 self.conf.tx_size as u64,
//                 &idx as *const u32 as *mut u32,
//             )
//         };

//         if ret != self.conf.tx_size as u64 {
//             return Err(format!(
//                 "error reserving prod ring of size {}, got {}",
//                 self.conf.tx_size, ret
//             ));
//         }
//         if self.inner.free_frame_addrs.len() < self.conf.tx_size as usize {
//             return Err(format!(
//                 "error: not enough free frames: expected {} got {}",
//                 self.conf.tx_size,
//                 self.inner.free_frame_addrs.len()
//             ));
//         }
//         for i in 0..self.conf.tx_size {
//             unsafe {
//                 *_xsk_ring_prod__fill_addr(
//                     &self.inner.umem_info.prod as *const xsk_ring_prod as *mut xsk_ring_prod,
//                     idx,
//                 ) = self.inner.free_frame_addrs.pop().ok_or(format!(
//                     "not enoug frames to fill the producer. Expected {} got {}",
//                     self.conf.tx_size, i
//                 ))?
//             };
//             idx += 1;
//         }

//         unsafe {
//             _xsk_ring_prod__submit(
//                 &self.inner.umem_info.prod as *const xsk_ring_prod as *mut xsk_ring_prod,
//                 self.conf.tx_size as u64,
//             )
//         };

//         Ok(*self.inner)
//     }

//     async fn build_async(mut self) -> Result<XskStream, String> {
//         let (addrs_tx, addrs_rx) = channel(self.inner.umem_info.n_frames as usize);
//         let poll_evented = PollEvented::new(XskSocketAsync {
//             inner: self.build()?,
//         })
//         .map_err(|e| format!("error creating poll evented: {}", e))?;

//         Ok(XskStream {
//             inner: poll_evented,
//             addrs_rx,
//             addrs_tx,
//         })
//     }
// }

// struct XskSocketAsync {
//     inner: XskSocketInfo,
// }

// impl Evented for XskSocketAsync {
//     fn register(
//         &self,
//         poll: &mio::Poll,
//         token: Token,
//         interest: Ready,
//         opts: PollOpt,
//     ) -> io::Result<()> {
//         EventedFd(&self.inner.get_raw_fd()).register(poll, token, interest, opts)
//     }

//     fn reregister(
//         &self,
//         poll: &mio::Poll,
//         token: Token,
//         interest: Ready,
//         opts: PollOpt,
//     ) -> io::Result<()> {
//         EventedFd(&self.inner.get_raw_fd()).reregister(poll, token, interest, opts)
//     }

//     fn deregister(&self, poll: &mio::Poll) -> io::Result<()> {
//         EventedFd(&self.inner.get_raw_fd()).deregister(poll)
//     }
// }

// struct XskPacket<'a> {
//     buf: &'a mut [u8],
//     addrs_tx: Sender<u64>,
// }

// impl<'a> XskPacket<'a> {
//     fn get_buf(&'a mut self) -> &'a mut [u8] {
//         self.buf
//     }
// }

// impl<'a> Drop for XskPacket<'a> {
//     fn drop(&mut self) {
//         // i think its ok to unwrap here, since at this point
//         // such an error cant be handled
//         match self.addrs_tx.try_send(self.buf.as_mut_ptr() as u64) {
//             Ok(()) => {}
//             Err(TrySendError::Closed(_)) => {}
//             Err(TrySendError::Full(_)) => {
//                 // this should never happen. since the size of the
//                 // bounded channel is the number of frames in umem
//                 // so we panic :)
//                 panic!("addrs channel is full")
//             }
//         }
//     }
// }

// struct XskStream {
//     inner: PollEvented<XskSocketAsync>,
//     addrs_rx: Receiver<u64>,
//     addrs_tx: Sender<u64>,
// }

// impl<'a> XskStream {
//     fn get_buf_by_idx(&'a mut self, idx: u32) -> Option<XskPacket<'a>> {
//         let addr = unsafe {
//             _xsk_ring_cons__rx_desc(
//                 &self.inner.get_ref().inner.cons_ring as *const xsk_ring_cons,
//                 idx,
//             )
//         };

//         if addr == std::ptr::null_mut() {
//             return None;
//         };

//         Some(XskPacket {
//             buf: unsafe {
//                 std::slice::from_raw_parts_mut((*addr).addr as *mut u8, (*addr).len as usize)
//             },
//             addrs_tx: self.addrs_tx.clone(),
//         })
//     }
// }

// struct ConsumerHalf<'a>(&'a XskStream);
// struct ProducerHalf<'a>(&'a XskStream);

// struct OwnedConsumerHalf {
//     inner: Arc<XskStream>,
// }

// impl<'a> Stream for &'a OwnedConsumerHalf {
//     type Item = io::Result<Vec<XskPacket<'a>>>;

//     fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> std::task::Poll<Option<Self::Item>> {
//         //this is the ready! macro from tokio .. i could not find a way to use it
//         match self.inner.inner.poll_read_ready(cx, mio::Ready::readable()) {
//             std::task::Poll::Ready(t) => t,
//             std::task::Poll::Pending => return std::task::Poll::Pending,
//         }?;

//         let cons = self.get_mut();

//         loop {
//             match cons.inner.addrs_rx.try_recv() {
//                 Ok(addr) => cons.inner.inner.get_ref().inner.free_frame_addrs.push(addr),
//             };
//         }

//         let rs_index = 0u32;

//         //let n_rcv = unsafe{ _xsk_ring_cons__peek(&self.inner.get_ref().inner.sock) }

//         std::task::Poll::Pending
//     }
// }

// struct OwnedProducerHalf(Arc<XskStream>);

// impl XskStream {
//     fn split(&mut self) -> (ConsumerHalf, ProducerHalf) {
//         (ConsumerHalf(self), ProducerHalf(self))
//     }

//     fn into_split(self) -> (OwnedConsumerHalf, OwnedProducerHalf) {
//         let arc = Arc::new(self);
//         (
//             OwnedConsumerHalf { inner: arc.clone() },
//             OwnedProducerHalf(arc),
//         )
//     }
// }

// #[cfg(test)]
// mod tests {
//     #[test]
//     fn basic_build_and_drop_umem() {
//         assert!(crate::XskUmemInfoBuilder::new().build().is_ok());
//     }

//     // having both sync and async tests running will cause the one that is ran second to fail
//     // idk why yet, but it seems like there should be some extra cleanup that we missed
//     // for now, we leave the async one because it actually contains the sync part

//     // after testing with --test-threads=1 and merging the two threads it is obvious that
//     // this is a cleanup problem

//     // #[test]
//     // #[cfg(feature = "bypass_link_xdp_id")]
//     // fn basic_build_and_drop_xsk_sync() {
//     //     let umem = crate::XskUmemInfoBuilder::new().build();
//     //     assert!(umem.is_ok());

//     //     assert!(
//     //         crate::XskSocketInfoBuilder::new(umem.unwrap(), "lo".to_string(), 0)
//     //             .with_libbpf_flag(crate::XSK_LIBBPF_FLAGS__INHIBIT_PROG_LOAD as u32)
//     //             .build()
//     //             .is_ok()
//     //     );
//     // }

//     #[test]
//     #[cfg(feature = "bypass_link_xdp_id")]
//     fn basic_build_and_drop_xsk_async() {
//         let umem = crate::XskUmemInfoBuilder::new().build();
//         assert!(umem.is_ok());
//         let mut rt = tokio::runtime::Builder::new()
//             .threaded_scheduler()
//             .enable_all()
//             .build()
//             .unwrap();
//         let e = rt.block_on(
//             crate::XskSocketInfoBuilder::new(umem.unwrap(), "lo".to_string(), 0)
//                 .with_libbpf_flag(crate::XSK_LIBBPF_FLAGS__INHIBIT_PROG_LOAD as u32)
//                 .build_async(),
//         );

//         assert!(e.is_ok());
//     }
// }

mod tests {
    #[test]
    fn invalid_fill_size() {
        let umem = crate::umem::XskUmemInfoBuilder::new().with_fill_size(3);

        assert!(umem.is_err());
    }

    #[test]
    fn valid_fill_size() {
        let umem = crate::umem::XskUmemInfoBuilder::new().with_fill_size(2048);

        assert!(umem.is_ok());
    }

    #[test]
    fn invalid_comp_size() {
        let umem = crate::umem::XskUmemInfoBuilder::new().with_comp_size(3);

        assert!(umem.is_err());
    }

    #[test]
    fn valid_comp_size() {
        let umem = crate::umem::XskUmemInfoBuilder::new().with_comp_size(2048);

        assert!(umem.is_ok());
    }

    #[test]
    fn valid_umem() {
        let umem = crate::umem::XskUmemInfoBuilder::new().build();

        assert!(umem.is_ok());
    }
}
