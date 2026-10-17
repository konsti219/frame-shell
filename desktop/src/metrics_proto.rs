// SessionFrame from WayVR’s Monado metrics schema (tags 15/16 carry submission timing).
#[derive(Clone, Copy, PartialEq, Eq, Hash, ::prost::Message)]
pub struct SessionFrame {
    #[prost(int64, tag = "1")]
    pub session_id: i64,
    #[prost(int64, tag = "2")]
    pub frame_id: i64,
    #[prost(uint64, tag = "3")]
    pub predicted_frame_time_ns: u64,
    #[prost(uint64, tag = "4")]
    pub predicted_wake_up_time_ns: u64,
    #[prost(uint64, tag = "5")]
    pub predicted_gpu_done_time_ns: u64,
    #[prost(uint64, tag = "6")]
    pub predicted_display_time_ns: u64,
    #[prost(uint64, tag = "7")]
    pub predicted_display_period_ns: u64,
    #[prost(uint64, tag = "8")]
    pub display_time_ns: u64,
    #[prost(uint64, tag = "9")]
    pub when_predicted_ns: u64,
    #[prost(uint64, tag = "10")]
    pub when_wait_woke_ns: u64,
    #[prost(uint64, tag = "11")]
    pub when_begin_ns: u64,
    #[prost(uint64, tag = "12")]
    pub when_delivered_ns: u64,
    #[prost(uint64, tag = "13")]
    pub when_gpu_done_ns: u64,
    #[prost(bool, tag = "14")]
    pub discarded: bool,
    #[prost(uint64, tag = "15")]
    pub when_submit_begin_ns: u64,
    #[prost(uint64, tag = "16")]
    pub when_submit_end_ns: u64,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Record {
    #[prost(message, optional, tag = "2")]
    pub session_frame: Option<SessionFrame>,
}

impl From<SessionFrame> for wayvr_ipc::frame::SessionFrame {
    fn from(f: SessionFrame) -> Self {
        Self {
            session_id: f.session_id,
            frame_id: f.frame_id,
            predicted_frame_time_ns: f.predicted_frame_time_ns,
            predicted_wake_up_time_ns: f.predicted_wake_up_time_ns,
            predicted_gpu_done_time_ns: f.predicted_gpu_done_time_ns,
            predicted_display_time_ns: f.predicted_display_time_ns,
            predicted_display_period_ns: f.predicted_display_period_ns,
            display_time_ns: f.display_time_ns,
            when_predicted_ns: f.when_predicted_ns,
            when_wait_woke_ns: f.when_wait_woke_ns,
            when_begin_ns: f.when_begin_ns,
            when_delivered_ns: f.when_delivered_ns,
            when_gpu_done_ns: f.when_gpu_done_ns,
            discarded: f.discarded,
            when_submit_begin_ns: f.when_submit_begin_ns,
            when_submit_end_ns: f.when_submit_end_ns,
        }
    }
}
