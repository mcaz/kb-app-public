//! 会話から独立した自動蒸留。複数プロセスの競合はコアの永続leaseが裁く。

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

use kb_core::{auto_distillation, distillation_ai, index, settings, vault::Vault};
use tauri::Manager;

use crate::state::AppState;

pub struct WorkerControl {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl WorkerControl {
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut running) = self.thread.lock()
            && let Some(handle) = running.take()
        {
            let _result = handle.join();
        }
    }
}

pub fn start(app: &tauri::AppHandle) -> WorkerControl {
    let app = app.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let running = thread::spawn(move || {
        while !stopped.load(Ordering::Acquire) {
            // 保存は別のMCPプロセスでも起こるため、メモリ内通知だけには頼らない。
            // Inboxの集約待ちと即時受付はコアの永続キューが判断する。
            if let Ok(config) = distillation_ai::load()
                && config.enabled
                && let Ok(kb) = settings::load()
                && distillation_ai::kb_enabled(&config, &kb)
                && distillation_ai::providers()
                    .iter()
                    .any(|p| Some(p.provider) == config.provider && p.unavailable_reason.is_none())
                && let Ok(root) = app.state::<AppState>().vault_root()
            {
                let cancelled = || {
                    let active = distillation_ai::load().ok();
                    let kb = settings::load().ok();
                    stopped.load(Ordering::Acquire)
                        || active.as_ref() != Some(&config)
                        || kb
                            .as_ref()
                            .is_none_or(|kb| !distillation_ai::kb_enabled(&config, kb))
                        || app
                            .state::<AppState>()
                            .vault_root()
                            .map_or(true, |current| current != root)
                };
                let result = (|| -> anyhow::Result<bool> {
                    if cancelled() {
                        return Ok(false);
                    }
                    let vault = Vault::open(&root)?;
                    let conn = index::open_db(&vault)?;
                    auto_distillation::run_next(&vault, &conn, &config, cancelled)
                })();
                if result.is_err() {
                    // 詳細は取得不能として状態コマンドにも現れる。KB本文をログへ出さない。
                    eprintln!("kb-app: 自動蒸留の状態を処理できなかった。次回再試行する");
                }
            }
            for _ in 0..20 {
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
    WorkerControl {
        stop,
        thread: Mutex::new(Some(running)),
    }
}
