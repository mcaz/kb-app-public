#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 同じ実行ファイルが MCP サーバーにもなる(詳細は mcp_mode)
    if kb_app_lib::mcp_mode::run_if_requested() {
        return;
    }
    kb_app_lib::run()
}
