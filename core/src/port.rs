//! 端口探测与等待（内置 node 服务就绪检测）。

use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use crate::{SERVER_HOST, SERVER_PORT};

/// 尝试连接一次 127.0.0.1:port，返回是否可连通。
pub fn is_port_open(port: u16, timeout_ms: u64) -> bool {
    let addr: SocketAddr = match format!("{SERVER_HOST}:{port}").parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(timeout_ms)).is_ok()
}

/// 轮询等待端口可用（最多 timeout_ms 毫秒）。
pub fn wait_for_port(port: u16, timeout_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        if is_port_open(port, 400) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    is_port_open(port, 600)
}

/// 默认端口探测。
pub fn default_port_open() -> bool {
    is_port_open(SERVER_PORT, 400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_port_is_closed() {
        // 挑一个几乎不可能被占用的端口
        assert!(!is_port_open(9, 300));
    }
}
