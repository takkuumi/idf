//! RS485 端口封装
//!
//! 利用 ESP-IDF UART 内置的 RS485 半双工模式 (UART_MODE_RS485_HALF_DUPLEX)，
//! 通过 RTS 引脚自动控制 DE/RE 信号，硬件连线把 DE+RE 同时接到 RTS。
//!
//! 本模块不使用 esp_idf_hal::uart::UartDriver，因为其对 RS485 模式封装不全。
//! 改为直接调用 ESP-IDF C API (uart_param_config / uart_set_pin / uart_set_mode)
//! 以获得完整控制。回调通过 esp_idf_sys::uart_write_bytes / uart_read_bytes。

use std::time::Duration;

use esp_idf_sys::*;

use crate::error::{AppError, AppResult};
use crate::rs485::config::Rs485Config;

const RX_BUF_SIZE: i32 = 256;
const RX_TIMEOUT_MS: u32 = 200;
#[allow(non_upper_case_globals)]
const portTICK_PERIOD_MS: u32 = 1000 / configTICK_RATE_HZ;

/// 单路 RS485 端口
pub struct Rs485Port {
    cfg: Rs485Config,
}

impl Rs485Port {
    /// 打开一路 RS485
    pub fn open(cfg: &Rs485Config) -> AppResult<Self> {
        // 1. 配置 UART 参数
        let mut uart_cfg = uart_config_t {
            baud_rate: cfg.baud as i32,
            data_bits: (cfg.data_bits - 5) as uart_parity_t,
            parity: match cfg.parity {
                'N' => uart_parity_t_UART_PARITY_DISABLE,
                'E' => uart_parity_t_UART_PARITY_EVEN,
                'O' => uart_parity_t_UART_PARITY_ODD,
                _ => uart_parity_t_UART_PARITY_DISABLE,
            },
            stop_bits: if cfg.stop_bits == 2 {
                uart_stop_bits_t_UART_STOP_BITS_2
            } else {
                uart_stop_bits_t_UART_STOP_BITS_1
            },
            flow_ctrl: uart_hw_flowcontrol_t_UART_HW_FLOWCTRL_DISABLE,
            __bindgen_anon_1: uart_config_t__bindgen_ty_1 {
                source_clk: soc_periph_uart_clk_src_legacy_t_UART_SCLK_DEFAULT,
            },
            ..Default::default()
        };

        let port = cfg.uart_port as uart_port_t;
        unsafe {
            uart_param_config(port, &mut uart_cfg);
        }

        // 2. 设置引脚 (TX/RX/CTS 不用, RTS 接到 DE)
        //    当 de_pin == 255 时不配置 RTS 引脚, 仅使用普通 UART (无 DE 控制)
        unsafe {
            let rts_pin = if cfg.de_pin != 255 { cfg.rts_pin as i32 } else { -1 };
            let r = uart_set_pin(
                port,
                cfg.tx_pin as i32,
                cfg.rx_pin as i32,
                rts_pin,              // RTS → DE (255 = 不配置)
                -1,                   // CTS 不用
            );
            check(r, "uart_set_pin")?;

            // 3. 安装驱动
            let r = uart_driver_install(port, RX_BUF_SIZE, RX_BUF_SIZE, 0, std::ptr::null_mut(), 0);
            check(r, "uart_driver_install")?;

            // 4. 当有 RTS/DE 引脚时, 切换为 RS485 半双工模式
            //    无 DE 引脚时保持默认 UART 模式 (仅 RX/TX, 无方向控制)
            if cfg.de_pin != 255 {
                let r = uart_set_mode(port, uart_mode_t_UART_MODE_RS485_HALF_DUPLEX);
                check(r, "uart_set_mode")?;
            }

            // 5. 启用硬件 RX 帧间隔检测 (Modbus RTU 3.5 字符时间)
            // 参数单位为字符时间 (11 bits/char), 3 表示 3 个字符静默即触发接收超时
            // 硬件会在帧结束时尽早返回 RX FIFO 数据, 显著降低主站响应延迟
            let r = uart_set_rx_timeout(port, 3);
            check(r, "uart_set_rx_timeout")?;
        }

        log::info!(
            "[rs485] opened uart{} @{} bps, tx={}, rx={}, de={}",
            cfg.uart_port, cfg.baud, cfg.tx_pin, cfg.rx_pin, cfg.de_pin
        );

        Ok(Self { cfg: *cfg })
    }

    /// 发送并等待应答 (主站用)
    pub fn send_recv(&mut self, request: &[u8], timeout_ms: u64) -> AppResult<heapless::Vec<u8, 256>> {
        self.write(request)?;
        std::thread::sleep(Duration::from_millis(2)); // 切换方向延迟
        let mut buf = [0u8; 256];
        let n = self.read(&mut buf, timeout_ms)?;
        let mut out = heapless::Vec::new();
        out.extend_from_slice(&buf[..n]).map_err(|_| AppError::Rs485("response too long".into()))?;
        Ok(out)
    }

    /// 仅发送
    pub fn write(&mut self, data: &[u8]) -> AppResult<()> {
        let port = self.cfg.uart_port as uart_port_t;
        let n = unsafe { uart_write_bytes(port, data.as_ptr() as *const _, data.len()) };
        if n != data.len() as i32 {
            return Err(AppError::Rs485(format!("write short: {n}/{}", data.len())));
        }
        // 等待发送完成 (RS485 模式下 RTS 自动在完成后释放)
        unsafe { uart_wait_tx_done(port, 100 / portTICK_PERIOD_MS); }
        Ok(())
    }

    /// 仅读取 (从站监听用)
    ///
    /// 采用两阶段读取策略, 符合 Modbus RTU 帧结束判定:
    /// 1. 第一阶段: 等待首字节 (长超时, 用于从站空闲监听)
    /// 2. 第二阶段: 帧间静默判断 (3.5 字符时间, 标准帧结束)
    ///
    /// 静默阈值: 11 bits/char * 3.5 char = 38.5 bits
    ///   - 9600bps:   ≈ 4.0ms
    ///   - 19200bps:  ≈ 2.0ms
    ///   - 115200bps: ≈ 0.33ms (最小限制 1ms, 保证实际可行)
    pub fn read(&mut self, buf: &mut [u8], timeout_ms: u64) -> AppResult<usize> {
        let port = self.cfg.uart_port as uart_port_t;
        let mut total = 0usize;
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);

        // 第一阶段: 等待首字节 (长超时, 利用硬件 RX 帧间隔检测尽早返回)
        while total == 0 && std::time::Instant::now() < deadline {
            let n = unsafe {
                uart_read_bytes(
                    port,
                    buf.as_mut_ptr() as *mut _,
                    buf.len() as u32,
                    RX_TIMEOUT_MS / portTICK_PERIOD_MS.max(1),
                )
            };
            if n > 0 {
                total = n as usize;
            }
        }

        if total == 0 {
            return Ok(0); // 超时无数据
        }

        // 第二阶段: 帧间静默判断
        // 3.5 字符时间 = 38.5 bits / 波特率, 最小 1ms 保证实时性
        let silence_us: u32 = 3_850_000u32 / self.cfg.baud.max(1);
        let silence_ms: u64 = std::cmp::max(silence_us / 1000, 1) as u64;
        let silence_dur = Duration::from_millis(silence_ms);

        while total < buf.len() && std::time::Instant::now() < deadline {
            // 短暂静默等待 (模拟 3.5 字符时间间隔)
            std::thread::sleep(silence_dur);

            // 检查 RX FIFO 中是否还有待读取数据 (使用标准 ESP-IDF API)
            let mut buffered: usize = 0;
            unsafe { uart_get_buffered_data_len(port, &mut buffered); }
            if buffered == 0 {
                // 静默超时, 帧结束
                break;
            }

            // 读取剩余数据 (立即返回, 不再阻塞等待)
            let n = unsafe {
                uart_read_bytes(
                    port,
                    buf[total..].as_mut_ptr() as *mut _,
                    (buf.len() - total) as u32,
                    1, // 1 tick: 立即返回已有数据
                )
            };
            if n > 0 {
                total += n as usize;
            } else {
                break;
            }
        }

        Ok(total)
    }

    /// 当前 UART 端口号
    pub fn port(&self) -> u8 {
        self.cfg.uart_port
    }
}

impl Drop for Rs485Port {
    fn drop(&mut self) {
        let port = self.cfg.uart_port as uart_port_t;
        unsafe { uart_driver_delete(port); }
    }
}

fn check(r: i32, ctx: &str) -> AppResult<()> {
    if r != 0 {
        Err(AppError::Rs485(format!("{ctx}: err={r:#x}")))
    } else {
        Ok(())
    }
}
