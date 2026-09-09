use std::process::Command;

pub const MIN_ONNX_CUDA_CC: (u32, u32) = (7, 0);

#[derive(Clone, Debug)]
pub struct GpuInfo {
    pub name: String,
    pub compute_capability: (u32, u32),
}

impl GpuInfo {
    pub fn supports_onnx_cuda(&self) -> bool {
        self.compute_capability >= MIN_ONNX_CUDA_CC
    }

    pub fn capability_string(&self) -> String {
        format!(
            "{}.{}",
            self.compute_capability.0, self.compute_capability.1
        )
    }
}

fn query(args: &[&str]) -> Option<String> {
    let output = Command::new("nvidia-smi").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn detect() -> Option<GpuInfo> {
    let stdout = query(&["--query-gpu=name,compute_cap", "--format=csv,noheader"])?;
    let line = stdout.lines().find(|line| !line.trim().is_empty())?;
    let (name, capability) = line.split_once(',')?;
    let (major, minor) = capability.trim().split_once('.')?;
    Some(GpuInfo {
        name: name.trim().to_string(),
        compute_capability: (major.trim().parse().ok()?, minor.trim().parse().ok()?),
    })
}

pub fn process_uses_gpu(pid: u32) -> Option<bool> {
    let stdout = query(&["--query-compute-apps=pid", "--format=csv,noheader"])?;
    Some(
        stdout
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .any(|found| found == pid),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_is_below_the_onnx_cuda_floor() {
        let pascal = GpuInfo {
            name: "NVIDIA GeForce GTX 1070 Ti".into(),
            compute_capability: (6, 1),
        };
        assert!(!pascal.supports_onnx_cuda());
        assert_eq!(pascal.capability_string(), "6.1");
    }

    #[test]
    fn turing_and_newer_clear_the_floor() {
        for capability in [(7, 0), (7, 5), (8, 6), (12, 0)] {
            let gpu = GpuInfo {
                name: "test".into(),
                compute_capability: capability,
            };
            assert!(gpu.supports_onnx_cuda(), "{capability:?} should be allowed");
        }
    }
}
