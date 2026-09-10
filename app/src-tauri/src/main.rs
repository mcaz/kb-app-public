#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if kb_app_lib::app_update_supervisor::run_before_app() {
        return;
    }
    if kb_app_lib::recovery_mode::run_if_requested() {
        return;
    }
    // 管理 lifecycle hook から、MCP だけを通る自動retrievalを実行する。
    if kb_app_lib::hook_mode::run_if_requested() {
        return;
    }
    // 同じ実行ファイルが MCP サーバーにもなる(詳細は mcp_mode)
    if kb_app_lib::mcp_mode::run_if_requested() {
        return;
    }
    kb_app_lib::run()
}
