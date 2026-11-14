//! RS485 端口封装
//!
//! 利用 ESP-IDF UART 内置的 RS485 半双工模式 (UART_MODE_RS485_HALF_DUPLEX)，
//! 通过 RTS 引脚自动控制 DE/RE 信号，硬件连线把 DE+RE 同时接到 RTS。
//!
//! 本模块不使用 esp_idf_hal::uart::UartDriver，因为其对 RS485 模式封装不全。
//! 改为直接调用 ESP-IDF C API (uart_param_config / uart_set_pin / uart_set_mode)
//! 以获得完整控制。回调通过 esp_idf_sys::uart_write_bytes / uart_read_bytes。

use esp_idf_sys::*;
use std::time::Duration;

use crate::error::{AppError, AppResult};
use crate::rs485::config::Rs485Config;
use crate::rs485::framing::{self, Direction, ReadPlan};

const RX_BUF_SIZE: i32 = 256;
// One maximum 256-byte RTU frame plus driver item metadata fits without
// waiting for the ISR to free the producer ring midway through a frame.
const TX_BUF_SIZE: i32 = 512;
const RX_TIMEOUT_MS: u32 = 200;
#[allow(non_upper_case_globals)]
const portTICK_PERIOD_MS: u32 = 1000 / configTICK_RATE_HZ;

#[inline]
fn ms_to_ticks(ms: u64) -> u32 {
    let period = portTICK_PERIOD_MS.max(1) as u64;
    ms.div_ceil(period).max(1).min(u32::MAX as u64) as u32
}

#[inline]
fn rtu_silence_us(baud: u32) -> u32 {
    super::timing::silence_us(baud)
}

#[inline]
fn rtu_silence_timeout_ms(baud: u32) -> u64 {
    super::timing::silence_timeout_ms(baud, u64::from(portTICK_PERIOD_MS.max(1)))
}

/// TX_DONE 超时保护余量，不是固定发送延迟。
/// 按两个字符时间覆盖 ISR/调度抖动和 UART FSM 状态更新；低速率保留
/// 足够余量，高速率缩短超时上限。正常发送通常提前返回，并非每帧固定等待该余量。
#[inline]
fn tx_guard_ms(baud: u32) -> u64 {
    let baud = u64::from(baud.max(1));
    let char_ms = 11_000u64.div_ceil(baud);
    char_ms.saturating_mul(2).clamp(3, 20)
}

/// 单路 RS485 端口
pub struct Rs485Port {
    cfg: Rs485Config,
}

impl Rs485Port {
    /// True only for the F3 LoRa UART0 transport. Other UARTs retain wired RTU framing.
    pub(crate) const fn uses_radio_framing(&self) -> bool {
        cfg!(all(feature = "f3", feature = "lora")) && self.cfg.uart_port == 0
    }

    /// 打开一路 RS485
    pub fn open(cfg: &Rs485Config) -> AppResult<Self> {
        // 1. 配置 UART 参数
        let uart_cfg = uart_config_t {
            baud_rate: cfg.baud as i32,
            data_bits: if cfg.data_bits == 7 {
                uart_word_length_t_UART_DATA_7_BITS
            } else {
                uart_word_length_t_UART_DATA_8_BITS
            },
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
            check(uart_param_config(port, &uart_cfg), "uart_param_config")?;
        }

        // 2. 设置引脚 (TX/RX/CTS 不用, RTS 接到 DE)
        //    当 de_pin == 255 时不配置 RTS 引脚, 仅使用普通 UART (无 DE 控制)
        unsafe {
            let rts_pin = if cfg.de_pin != 255 {
                cfg.rts_pin as i32
            } else {
                -1
            };
            let r = uart_set_pin(
                port,
                cfg.tx_pin as i32,
                cfg.rx_pin as i32,
                rts_pin, // RTS → DE (255 = 不配置)
                -1,      // CTS 不用
            );
            check(r, "uart_set_pin")?;

            // 3. 安装驱动
            let r = uart_driver_install(port, RX_BUF_SIZE, TX_BUF_SIZE, 0, std::ptr::null_mut(), 0);
            check(r, "uart_driver_install")?;

            // 4. 当有 RTS/DE 引脚时, 切换为 RS485 半双工模式
            //    无 DE 引脚时保持默认 UART 模式 (仅 RX/TX, 无方向控制)
            if cfg.de_pin != 255 {
                let r = uart_set_mode(port, uart_mode_t_UART_MODE_RS485_HALF_DUPLEX);
                if let Err(e) = check(r, "uart_set_mode") {
                    let _ = uart_driver_delete(port);
                    return Err(e);
                }
            }

            // Hardware FIFO delivery must not hide active bytes longer than
            // the software inter-byte gap. Scale batching with baud rate.
            let char_bits = 1
                + u32::from(cfg.data_bits)
                + u32::from(cfg.parity != 'N')
                + u32::from(cfg.stop_bits);
            let threshold = super::timing::rx_fifo_threshold(cfg.baud, char_bits);
            let r = uart_set_rx_full_threshold(port, threshold as i32);
            if let Err(e) = check(r, "uart_set_rx_full_threshold") {
                let _ = uart_driver_delete(port);
                return Err(e);
            }

            // 5. 启用硬件 RX 帧间隔检测 (Modbus RTU 3.5 字符时间)
            // 参数单位为字符时间 (11 bits/char), 3 表示 3 个字符静默即触发接收超时
            // 硬件会在帧结束时尽早返回 RX FIFO 数据, 显著降低主站响应延迟
            let r = uart_set_rx_timeout(port, 3);
            if let Err(e) = check(r, "uart_set_rx_timeout") {
                let _ = uart_driver_delete(port);
                return Err(e);
            }
        }

        log::info!(
            "[rs485] opened uart{} @{} bps, tx={}, rx={}, de={}",
            cfg.uart_port,
            cfg.baud,
            cfg.tx_pin,
            cfg.rx_pin,
            cfg.de_pin
        );

        super::diagnostics::opened(
            cfg.uart_port,
            cfg.baud,
            super::timing::rx_fifo_threshold(
                cfg.baud,
                1 + u32::from(cfg.data_bits)
                    + u32::from(cfg.parity != 'N')
                    + u32::from(cfg.stop_bits),
            ),
            TX_BUF_SIZE as u32,
        );
        Ok(Self { cfg: *cfg })
    }

    /// 发送并等待应答 (主站用)
    pub fn send_recv(
        &mut self,
        request: &[u8],
        timeout_ms: u64,
    ) -> AppResult<heapless::Vec<u8, 256>> {
        self.send_recv_with_first_byte_timeout(request, timeout_ms, timeout_ms)
    }

    /// 发送并等待应答，同时把“等待首字节”和“接收完整帧”分开计时。
    ///
    /// 现场没有从站时，首字节等待是主站最主要的阻塞来源；收到首字节后，
    /// 再按完整帧预算等待剩余字节，避免低波特率或长响应被过早截断。
    pub fn send_recv_with_first_byte_timeout(
        &mut self,
        request: &[u8],
        first_byte_timeout_ms: u64,
        frame_timeout_ms: u64,
    ) -> AppResult<heapless::Vec<u8, 256>> {
        // 上一轮超时后可能有迟到的响应残留在 RX ring。Modbus 主站在发出新请求前
        // 没有合法的未消费数据，清空可避免旧帧被误配给本轮事务。
        check(
            unsafe { uart_flush_input(self.cfg.uart_port as uart_port_t) },
            "uart_flush_input",
        )?;
        self.write(request)?;
        let mut buf = [0u8; 256];
        let effective_frame_timeout = if self.uses_radio_framing() {
            frame_timeout_ms.max(framing::RADIO_FRAME_TIMEOUT_MS)
        } else {
            frame_timeout_ms
        };
        let n = if self.uses_radio_framing() {
            self.read_length_framed(
                &mut buf,
                first_byte_timeout_ms,
                effective_frame_timeout,
                Direction::Response,
            )?
        } else {
            self.read_with_first_byte_timeout(&mut buf, first_byte_timeout_ms, frame_timeout_ms)?
        };
        let mut out = heapless::Vec::new();
        out.extend_from_slice(&buf[..n])
            .map_err(|_| AppError::Rs485("response too long".into()))?;
        Ok(out)
    }

    /// 仅发送
    pub fn write(&mut self, data: &[u8]) -> AppResult<()> {
        let started = std::time::Instant::now();
        let result = (|| {
            let port = self.cfg.uart_port as uart_port_t;
            let n = unsafe { uart_write_bytes(port, data.as_ptr() as *const _, data.len()) };
            if n != data.len() as i32 {
                return Err(AppError::Rs485(format!("write short: {n}/{}", data.len())));
            }
            // 等待发送完成 (RS485 模式下 RTS 自动在完成后释放)
            let parity_bits = u64::from(self.cfg.parity != 'N');
            let bits_per_char =
                1 + self.cfg.data_bits as u64 + parity_bits + self.cfg.stop_bits as u64;
            let tx_ms = (data.len() as u64)
                .saturating_mul(bits_per_char)
                .saturating_mul(1000)
                .div_ceil(self.cfg.baud.max(1) as u64)
                .saturating_add(tx_guard_ms(self.cfg.baud))
                .min(5000);
            check(
                unsafe { uart_wait_tx_done(port, ms_to_ticks(tx_ms)) },
                "uart_wait_tx_done",
            )?;
            Ok(())
        })();
        super::diagnostics::tx(self.cfg.uart_port, data, started.elapsed(), result.is_ok());
        result
    }

    /// 仅读取 (从站监听用)
    ///
    /// 采用两阶段读取策略, 符合 Modbus RTU 帧结束判定:
    /// 1. 第一阶段: 等待首字节 (由调用方传入，用于从站空闲监听)
    /// 2. 第二阶段: 帧间静默判断 (3.5 字符时间, 标准帧结束)
    ///
    /// 静默阈值: 11 bits/char * 3.5 char = 38.5 bits
    ///   - 9600bps:   ≈ 4.0ms
    ///   - 19200bps:  ≈ 2.0ms
    ///   - 115200bps: ≈ 0.33ms (最小限制 1ms, 保证实际可行)
    pub fn read(&mut self, buf: &mut [u8], timeout_ms: u64) -> AppResult<usize> {
        let frame_ms = self.transaction_timeout_ms(0, buf.len(), timeout_ms);
        if self.uses_radio_framing() {
            self.read_length_framed(
                buf,
                timeout_ms,
                frame_ms.max(framing::RADIO_FRAME_TIMEOUT_MS),
                Direction::Request,
            )
        } else {
            self.read_with_first_byte_timeout(buf, timeout_ms, frame_ms)
        }
    }

    /// UART0 carries E22 transparent-radio packets, whose inter-packet gaps do
    /// not mark an RTU frame boundary. Read supported Modbus frames to their
    /// declared length; keep wired UARTs on the standard t3.5 path above.
    fn read_length_framed(
        &mut self,
        buf: &mut [u8],
        first_byte_timeout_ms: u64,
        frame_timeout_ms: u64,
        direction: Direction,
    ) -> AppResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let port = self.cfg.uart_port as uart_port_t;
        let first_deadline =
            std::time::Instant::now() + Duration::from_millis(first_byte_timeout_ms.max(1));
        let mut total = 0usize;
        let mut byte = [0u8; 1];
        while total == 0 && std::time::Instant::now() < first_deadline {
            crate::health::feed_wdt();
            let remaining_ms = first_deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_millis()
                .max(1)
                .min(RX_TIMEOUT_MS as u128) as u64;
            let n = unsafe {
                uart_read_bytes(
                    port,
                    byte.as_mut_ptr() as *mut _,
                    1,
                    ms_to_ticks(remaining_ms),
                )
            };
            if n < 0 {
                return Err(AppError::Rs485(format!("uart_read_bytes: {n}")));
            }
            if n > 0 {
                buf[0] = byte[0];
                total = 1;
            }
        }
        if total == 0 {
            return Ok(0);
        }

        let frame_started = std::time::Instant::now();
        let frame_deadline = frame_started + Duration::from_millis(frame_timeout_ms.max(1));
        loop {
            crate::health::feed_wdt();
            match framing::read_plan(&buf[..total], direction, buf.len()) {
                ReadPlan::Complete => break,
                ReadPlan::NeedBytes(missing) => {
                    let remaining_ms = frame_deadline
                        .saturating_duration_since(std::time::Instant::now())
                        .as_millis()
                        .min(u128::from(u64::MAX)) as u64;
                    if remaining_ms == 0 {
                        break;
                    }
                    let requested = missing.min(buf.len() - total);
                    let wait_ms = framing::RADIO_PACKET_GAP_TIMEOUT_MS
                        .min(remaining_ms)
                        .max(1);
                    let n = unsafe {
                        uart_read_bytes(
                            port,
                            buf[total..total + requested].as_mut_ptr() as *mut _,
                            requested as u32,
                            ms_to_ticks(wait_ms),
                        )
                    };
                    if n < 0 {
                        return Err(AppError::Rs485(format!("uart_read_bytes: {n}")));
                    }
                    if n == 0 {
                        break;
                    }
                    total += n as usize;
                }
                ReadPlan::GapDelimited => {
                    while total < buf.len() {
                        let remaining =
                            frame_deadline.saturating_duration_since(std::time::Instant::now());
                        let wait_ticks = super::timing::inter_byte_wait_ticks(
                            self.cfg.baud,
                            remaining.as_micros().min(u128::from(u64::MAX)) as u64,
                            u64::from(portTICK_PERIOD_MS.max(1)),
                        );
                        if wait_ticks == 0 {
                            break;
                        }
                        let n = unsafe {
                            uart_read_bytes(
                                port,
                                buf[total..].as_mut_ptr() as *mut _,
                                1,
                                wait_ticks,
                            )
                        };
                        if n < 0 {
                            return Err(AppError::Rs485(format!("uart_read_bytes: {n}")));
                        }
                        if n == 0 {
                            break;
                        }
                        total += n as usize;
                    }
                    break;
                }
            }
        }
        super::diagnostics::rx(self.cfg.uart_port, total, frame_started.elapsed());
        Ok(total)
    }

    /// 先快速等待首字节，收到后再开启完整帧超时预算。
    pub fn read_with_first_byte_timeout(
        &mut self,
        buf: &mut [u8],
        first_byte_timeout_ms: u64,
        frame_timeout_ms: u64,
    ) -> AppResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let port = self.cfg.uart_port as uart_port_t;
        let mut total = 0usize;
        let first_deadline =
            std::time::Instant::now() + Duration::from_millis(first_byte_timeout_ms.max(1));

        // 第一阶段: 只请求 1 字节，等待首字节。
        // uart_read_bytes 会尝试满足调用方要求的整个长度。若请求 buf.len()
        // (从站传入 256)，一个 8 字节请求虽然已完整进入 ring buffer，函数仍
        // 会继续等到 250ms 超时才返回不足量，导致 PLC 必须把轮询间隔设很大。
        // 单字节请求在数据可用时立即返回，后续由 3.5 字符间隔逻辑收齐整帧。
        while total == 0 && std::time::Instant::now() < first_deadline {
            let remaining_ms = first_deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_millis()
                .max(1)
                .min(RX_TIMEOUT_MS as u128) as u64;
            let mut first_byte = [0u8; 1];
            let n = unsafe {
                uart_read_bytes(
                    port,
                    first_byte.as_mut_ptr() as *mut _,
                    1,
                    ms_to_ticks(remaining_ms),
                )
            };
            if n < 0 {
                return Err(AppError::Rs485(format!("uart_read_bytes: {n}")));
            }
            if n > 0 {
                buf[0] = first_byte[0];
                total = 1;
            }
        }

        if total == 0 {
            return Ok(0); // 超时无数据
        }

        // 收到首字节后，重新开始完整帧预算。这样 1200/2400bps 的合法长响应
        // 不会被“首字节快速失败”策略截断。
        let frame_started = std::time::Instant::now();
        let frame_deadline = frame_started + Duration::from_millis(frame_timeout_ms.max(1));

        // 第二阶段: 帧间静默判断。不要用 sleep + get_buffered_data_len
        // 判断帧结束：UART 驱动按 FIFO/RX timeout 事件分段投递 ring buffer，
        // 两个事件之间的短暂空窗会被误判为完整帧，低速 PLC 随后收到短帧
        // 或 CRC 错误，只能把下一轮间隔拉到约 200ms 才偶尔成功。
        // 逐字节读取并使用 3.5 字符作为 inter-byte timeout，可跨越事件分段，
        // 真正静默后立即返回，不增加固定等待。
        // 3.5 字符时间 = 38.5 bits / 波特率。向上取整到 FreeRTOS tick，
        // 不能早于 RTU 帧间静默边界结束接收。
        while total < buf.len() {
            let remaining = frame_deadline.saturating_duration_since(std::time::Instant::now());
            let wait_ticks = super::timing::inter_byte_wait_ticks(
                self.cfg.baud,
                remaining.as_micros().min(u128::from(u64::MAX)) as u64,
                u64::from(portTICK_PERIOD_MS.max(1)),
            );
            if wait_ticks == 0 {
                break;
            }
            let mut byte = [0u8; 1];
            let n = unsafe { uart_read_bytes(port, byte.as_mut_ptr() as *mut _, 1, wait_ticks) };
            if n < 0 {
                return Err(AppError::Rs485(format!("uart_read_bytes: {n}")));
            }
            if n == 0 {
                // 3.5 字符静默，完整帧已经接收。
                break;
            }
            buf[total] = byte[0];
            total += 1;
        }

        super::diagnostics::rx(self.cfg.uart_port, total, frame_started.elapsed());
        Ok(total)
    }

    /// 当前 UART 端口号
    pub fn port(&self) -> u8 {
        self.cfg.uart_port
    }

    /// 根据请求/响应长度估算一笔 RTU 事务所需的完整帧时间。
    ///
    /// 该值只用于“收到首字节之后”的帧接收预算；没有从站时仍由首字节
    /// 快速超时控制等待时间。保留调用方配置的更大超时，兼容慢速仪表。
    pub fn transaction_timeout_ms(
        &self,
        request_len: usize,
        response_len: usize,
        configured_timeout_ms: u64,
    ) -> u64 {
        let parity_bits = u32::from(self.cfg.parity != 'N');
        let bits_per_char =
            1 + u32::from(self.cfg.data_bits) + parity_bits + u32::from(self.cfg.stop_bits);
        super::timing::frame_timeout_ms(
            self.cfg.baud,
            bits_per_char,
            request_len,
            response_len,
            configured_timeout_ms,
            u64::from(portTICK_PERIOD_MS.max(1)),
        )
    }
}

impl Drop for Rs485Port {
    fn drop(&mut self) {
        super::diagnostics::closed(self.cfg.uart_port);
        let port = self.cfg.uart_port as uart_port_t;
        unsafe {
            uart_driver_delete(port);
        }
    }
}

fn check(r: i32, ctx: &str) -> AppResult<()> {
    if r != 0 {
        Err(AppError::Rs485(format!("{ctx}: err={r:#x}")))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modbus_rtu_silence_uses_3_5_character_times() {
        assert_eq!(rtu_silence_us(9600), 4011);
        assert_eq!(rtu_silence_us(19200), 2006);
        assert_eq!(rtu_silence_us(115200), 1750);
    }

    #[test]
    fn test_rtu_silence_timeout_rounds_up_to_scheduler_tick() {
        let tick_ms = u64::from(portTICK_PERIOD_MS.max(1));
        for baud in [1200, 9600, 19200, 115200, 921_600] {
            let timeout_ms = rtu_silence_timeout_ms(baud);
            assert!(timeout_ms * 1000 >= u64::from(rtu_silence_us(baud)));
            assert_eq!(timeout_ms % tick_ms, 0);
        }
        assert_eq!(rtu_silence_timeout_ms(9600), 5);
    }

    #[test]
    fn test_tick_conversion_never_returns_zero() {
        assert_eq!(ms_to_ticks(0), 1);
        assert!(ms_to_ticks(100) >= 1);
    }

    #[test]
    fn test_transaction_timeout_is_bounded_and_frame_aware() {
        let port = Rs485Port {
            cfg: Rs485Config {
                baud: 9_600,
                data_bits: 8,
                parity: 'N',
                stop_bits: 1,
                ..Rs485Config::default()
            },
        };
        let timeout = port.transaction_timeout_ms(8, 255, 100);
        assert!(timeout >= 100);
        assert!(timeout < 5000);
        assert_eq!(port.transaction_timeout_ms(8, 5, 60_000), 2000);
        // This unit test does not open a driver; avoid Drop touching UART0.
        std::mem::forget(port);
    }

    #[test]
    fn test_tx_guard_scales_with_baud_without_zero_margin() {
        assert_eq!(tx_guard_ms(1200), 20);
        assert_eq!(tx_guard_ms(9600), 4);
        assert_eq!(tx_guard_ms(115_200), 3);
        assert_eq!(tx_guard_ms(921_600), 3);
    }
}
