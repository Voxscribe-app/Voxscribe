pub mod asr;
pub mod audio;
pub mod cli;
pub mod core;
pub mod daemon;
pub mod input;
pub mod integrations;
pub mod ipc;
pub mod migrate;
pub mod models;
pub mod text;
pub mod translate;

#[test]
#[ignore]
fn osd_cycle() {
    use crate::core::config::{Osd as OsdConfig, OsdMode};
    use crate::core::state::Phase;
    let config = OsdConfig {
        enabled: OsdMode::On,
        ..OsdConfig::default()
    };
    let osd = crate::integrations::osd::Osd::start(&config).unwrap();
    for cycle in 0..3 {
        eprintln!("cycle {cycle}: recording");
        let start = std::time::Instant::now();
        let mut t = 0.0f32;
        while start.elapsed() < std::time::Duration::from_secs(3) {
            osd.update(Phase::Recording, 0.4 + 0.3 * (t / 5.0).sin());
            std::thread::sleep(std::time::Duration::from_millis(40));
            t += 1.0;
        }
        eprintln!("cycle {cycle}: idle");
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(2) {
            osd.update(Phase::Idle, 0.0);
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    }
}
