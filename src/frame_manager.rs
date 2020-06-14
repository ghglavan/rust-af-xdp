use tokio::sync::mpsc::<Receiver, Sender>;

pub type Frame = u64;

struct XskFrameManager {
    n_frames: u64,
    frame_size: u32,
    
    frames: Vec<Frame>,

    frame_tx: Sender<Frame>
    frame_rx: Receiver<Frame>
}

impl XskFrameManager
