use std::ffi::CString;
use wayvr_ipc::runtime::Runtime;

#[derive(Default, Debug, PartialEq, Eq)]
pub(super) struct Capture {
    pub profiles: [String; 2],
    pub paths: Vec<String>,
    pub pose_mask: u32,
}

impl Capture {
    pub fn add(&mut self, path: &str) {
        let (base, component) = path.rsplit_once('/').unwrap_or((path, ""));
        match component {
            "click" | "value" | "force" => {
                for alias in ["click", "value", "force"] {
                    self.paths.push(format!("{base}/{alias}"));
                }
            }
            "x" | "y" => self.paths.push(base.to_owned()),
            _ => self.paths.push(path.to_owned()),
        }
    }
}

pub(super) struct InputBlocker {
    runtime: Runtime,
    previous: Option<Capture>,
}

impl InputBlocker {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            runtime: Runtime::connect()?,
            previous: None,
        })
    }

    pub fn update(&mut self, capture: Capture) -> anyhow::Result<()> {
        if self.previous.as_ref() == Some(&capture) {
            return Ok(());
        }
        let profiles = [
            CString::new(capture.profiles[0].as_str())?,
            CString::new(capture.profiles[1].as_str())?,
        ];
        let paths: Vec<_> = capture
            .paths
            .iter()
            .map(|p| CString::new(p.as_str()))
            .collect::<Result<_, _>>()?;
        self.runtime
            .set_input_capture(&profiles, &paths, capture.pose_mask)?;
        crate::frame_shell::set_input_capture(&paths, capture.pose_mask);
        log::trace!("Input capture: {:?}", capture);
        self.previous = Some(capture);
        Ok(())
    }
}

impl Drop for InputBlocker {
    fn drop(&mut self) {
        crate::frame_shell::set_input_capture(&[], 0);
    }
}
