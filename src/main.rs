use rust_af_xdp::*;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::prelude::*;
use tokio::task;

#[tokio::main]
async fn main() {
    let umem = umem::XskUmemInfoBuilder::new()
        .with_fill_size(4)
        .unwrap()
        .with_comp_size(2)
        .unwrap()
        .build();
    assert!(umem.is_ok());

    let xsk = xsk_stream::XskSocketInfoBuilder::new(umem.unwrap(), "lo".to_string(), 0u32)
        .with_rx_batch_size(4)
        .build_async()
        .await;
    assert!(xsk.is_ok());
    let mut xsk = xsk.unwrap();

    let size = xsk.get_buf_size();

    let mut v: Vec<u8> = vec![0; size];

    xsk.reserve_frames_for_fill_ring(2).unwrap();

    println!("receiving");

    let local = task::LocalSet::new();

    local
        .run_until(async move {
            let xsk_rc = Rc::new(RefCell::new(xsk));

            for _ in 0..10 {
                let rc = xsk_rc.clone();
                let mut vc = v.clone();
                task::spawn_local(async move {
                    println!("recving async");
                    let s = vc.as_mut_slice();
                    let r = (*rc.borrow_mut()).read(s).await.unwrap();

                    println!("got {} bytes", r);
                    if r != 0 {
                        let mut r = rc.borrow_mut();
                        let p = (*r).get_packet_by_addr_buf(s);
                        (*r).clean_packet(p);
                    }
                })
                .await
                .unwrap()
            }
        })
        .await;
}
