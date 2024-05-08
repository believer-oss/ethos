#[derive(Debug, Clone)]
pub enum LongtailMsg {
    Log(String),
    ExecEvt(String),
    ErrEvt(String),
    DoneLtDlEvt,
    DoneArcSyncEvt,
}
