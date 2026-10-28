[2026-10-28T03:14:02Z INFO ] Using flash stub
Chip type:         esp32s3 (revision v0.2)
Crystal frequency: 40 MHz
Flash size:        8MB
Features:          WiFi, BLE, Embedded Flash
MAC address:       fc:01:2c:05:64:88
[2026-10-28T03:14:03Z INFO ] Erasing otadata (Data(Ota))...
App/part. size:    1,744,704/2,359,296 bytes, 73.95%
[00:00:01] [========================================]       1/1       0x0      Verifying... OK!
[00:00:00] [========================================]       1/1       0x8000   Verifying... OK!
[00:01:44] [========================================]      59/59      0x20000  Verifying... OK!                               [2026-10-28T03:15:50Z INFO ] Flashing has completed!
Commands:
    CTRL+R    Reset chip
    CTRL+C    Exit

ESP-ROM:esp32s3-20210327
Build:Mar 27 2021
rst:0x1 (POWERON),boot:0x2b (SPI_FAST_FLASH_BOOT)
SPIWP:0xee
mode:DIO, clock div:2
load:0x3fce2820,len:0x14f0
load:0x403c8700,len:0xd24
load:0x403cb700,len:0x2f48
entry 0x403c8924
I (29) boot: ESP-IDF v5.5.4 2nd stage bootloader
I (29) boot: compile time Oct 14 2026 23:48:56
I (30) boot: Multicore bootloader
I (30) boot: chip revision: v0.2
I (33) boot: efuse block revision: v1.3
I (36) boot.esp32s3: Boot SPI Speed : 40MHz
I (40) boot.esp32s3: SPI Mode       : DIO
I (44) boot.esp32s3: SPI Flash Size : 8MB
I (48) boot: Enabling RNG early entropy source...
I (52) boot: Partition Table:
I (55) boot: ## Label            Usage          Type ST Offset   Length
I (61) boot:  0 nvs              WiFi data        01 02 00009000 00006000
I (67) boot:  1 phy_init         RF data          01 01 0000f000 00001000
I (74) boot:  2 otadata          OTA data         01 00 00010000 00002000
I (81) boot:  3 nvs_keys         NVS keys         01 04 00017000 00001000
I (87) boot:  4 holding          Unknown data     01 06 00018000 00008000
I (94) boot:  5 factory          factory app      00 00 00020000 00240000
I (100) boot:  6 ota_0            OTA app          00 10 00260000 00240000
I (107) boot:  7 ota_1            OTA app          00 11 004a0000 00240000
I (113) boot:  8 coredump         Unknown data     01 03 006e0000 00010000
I (120) boot:  9 ble_mesh         WiFi data        01 02 006f0000 00010000
I (126) boot: 10 storage          Unknown data     01 81 00700000 00100000
I (133) boot: End of partition table
I (136) boot: Defaulting to factory image
I (140) esp_image: segment 0: paddr=00020020 vaddr=3c130020 size=5eaech (387820) map
I (243) esp_image: segment 1: paddr=0007eb14 vaddr=3fca4000 size=01504h (  5380) load
I (245) esp_image: segment 2: paddr=00080020 vaddr=42000020 size=12875ch (1214300) map
I (545) esp_image: segment 3: paddr=001a8784 vaddr=3fca5504 size=057b8h ( 22456) load
I (551) esp_image: segment 4: paddr=001adf44 vaddr=40378000 size=1bfach (114604) load
I (584) esp_image: segment 5: paddr=001c9ef8 vaddr=50000000 size=00020h (    32) load
I (595) boot: Loaded app from partition at offset 0x20000
I (596) boot: Disabling RNG early entropy source...
I (606) esp_psram: Found 2MB PSRAM device
I (606) esp_psram: Speed: 80MHz
I (607) cpu_start: Multicore app
I (801) esp_psram: SPI SRAM memory test OK
I (810) cpu_start: GPIO 44 and 43 are used as console UART I/O pins
I (811) cpu_start: Pro cpu start user code
I (811) cpu_start: cpu freq: 240000000 Hz
I (815) app_init: Application information:
I (819) app_init: Project name:     libespidf
I (825) app_init: App version:      2.2.7
I (829) app_init: Compile time:     Oct 14 2026 23:48:17
I (835) app_init: ELF file SHA256:  a2143d798...
I (840) app_init: ESP-IDF:          v5.5.4
I (845) efuse_init: Min chip rev:     v0.0
I (850) efuse_init: Max chip rev:     v0.99
I (855) efuse_init: Chip rev:         v0.2
I (860) heap_init: Initializing. RAM available for dynamic allocation:
I (867) heap_init: At 3FCB1A60 len 00037CB0 (223 KiB): RAM
I (873) heap_init: At 3FCE9710 len 00005724 (21 KiB): RAM
I (879) heap_init: At 3FCF0000 len 00008000 (32 KiB): DRAM
I (885) heap_init: At 600FE000 len 00001FE8 (7 KiB): RTCRAM
I (892) esp_psram: Adding pool of 2048K of PSRAM memory to heap allocator
I (901) spi_flash: detected chip: winbond
I (904) spi_flash: flash io: dio
I (909) sleep_gpio: Configure to isolate all GPIO pins in sleep state
I (915) sleep_gpio: Enable automatic switching of GPIO sleep configuration
I (923) esp_core_dump_flash: Init core dump to flash
I (929) esp_core_dump_flash: Found partition 'coredump' @ 6e0000 65536 bytes
D (936) esp_core_dump_flash: Blank core dump partition!
I (941) coexist: coex firmware version: 093e3d2
I (946) coexist: coexist rom version e7ae62f
I (952) main_task: Started on CPU0
I (956) esp_psram: Reserving pool of 64K of internal memory for DMA/internal allocations
I (965) main_task: Calling app_main()
I (970) gateway: ================================================
I (975) gateway: esp32s3-iot-gateway v2.2.7
I (980) gateway: [build] id=fd89b4ca1ec9
I (985) gateway: ESP32-S3R2 IoT Gateway starting...
I (990) gateway: ================================================
I (1001) gateway::hal::adc::adc_continuous: [adc] Continuous+DMA mode started: 6 ch, 1kHz total, Type2 12-bit
I (1060) gateway::hal: [hal] IO/LED board power enabled (gpio21 + gpio33 HIGH)
I (1061) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (1064) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (1072) gateway::hal::pca9555: [pca9555] io bus: DI@0x40(input) DO@0x42(output)
I (1083) gateway::hal::pca9555: [pca9555] status LEDs: PWR+RUN on (0xDE)
I (1085) gateway::hal::pca9555: [pca9555] led bus: all chips ready, all LEDs off except PWR/RUN
I (1095) gateway::hal::pca9555: [pca9555] F16 (16DI+16DO) init complete
I (1102) gateway: [main] initializing NVS flash...
I (1117) gateway::device: [device] NVS namespace 'gateway' opened
I (1118) gateway: [main] reset reason=1 (POWERON) previous=0 (UNKNOWN)
I (1122) gateway::rs485::dip: [dip] AD0=0 AD1=0 AD2=0 AD3=0 → address=0 (gpio 19/20/48/47)
I (1130) gateway: [main] starting device protocol store...
I (1137) gateway::device: [device] no active flag, trying legacy single blob
I (1144) gateway::device: [device] nvs empty or all blobs corrupted, using defaults
I (1153) gateway::device: [device] dev_text blob not present, using empty
I (1160) gateway::device: [device] no valid dev_text blob, using empty
I (1168) gateway::device::system_config: [cfg] nvs empty, using defaults
I (1176) gateway::device::system_config: [cfg] eth_mac loaded from hw: FC:01:2C:05:64:8B
I (1184) gateway::device::system_config: [cfg] ble_mac loaded from hw: FC:01:2C:05:64:8A
I (1193) gateway::device::holding_store: [holding] raw partition found: offset=0x18000 size=32KB
W (1205) gateway::device::holding_store: [holding] no valid persistent snapshot; using empty configuration until project sync
I (1216) gateway::device: [device] do_bits magic not found or wrong (0x0), default 0
I (1222) gateway::device: [device] DO bits loaded: 0x0000000000000000
I (1229) gateway::device: [device] proto loaded: 0 words, version=0x0000
I (1237) gateway::device: [device] cfg loaded: sn='ESP32-001' ip=192.168.51.226 mac=FC:01:2C:05:64:8B fw=0x00E3
I (1249) gateway::device: [device] DeviceActor started (mailbox consumer thread)
I (1256) gateway: [main] device init ok
I (1260) gateway: [main] reset count=1 reason=1 (POWERON)
I (1266) gateway: [main] starting ethernet (W5500)...
I (1272) gateway::ethernet::w5500: [eth] initializing W5500 over SPI2 (MISO=11,MOSI=12,SCLK=10,CS=9,INT=13,RST=14)...
I (1284) gateway::ethernet::w5500: [eth] resetting W5500 via GPIO14...
I (1342) gateway::ethernet::w5500: [eth] SPI bus initialized (host=2, DMA=auto)
I (1355) gateway::ethernet::w5500: [eth] setting MAC: FC:01:2C:05:64:8B
I (1357) esp_eth.netif.netif_glue: fc:01:2c:05:64:8b
I (1357) esp_eth.netif.netif_glue: ethernet attached to netif
I (1364) gateway::ethernet::w5500: [eth] network config: dhcp=false ip=192.168.51.226 mask=255.255.255.0 gw=192.168.51.1 dns=192.168.51.1
I (1379) gateway::ethernet::w5500: [eth] static IP configured: 192.168.51.226 / 255.255.255.0 gw 192.168.51.1 dns 192.168.51.1
I (1389) gateway::ethernet::w5500: [eth] IP/ETH event handlers registered
I (1406) gateway::ethernet::w5500: [eth] W5500 driver installed and started, eth_handle=0x3fcd0b88
I (1407) gateway::ethernet::w5500: [eth] heartbeat registered in main_loop (period=5s)
I (1414) gateway::ethernet::w5500: [eth] W5500 startup complete
I (1421) gateway: [main] starting ble gatt server...
I (1427) gateway::ble_at: [ble_at] service starting (uuid=4fafc201-1fb5-459e-8fcc-c5c9c331914b)
I (1436) gateway::ble_at: [ble_at] initializing BT controller (BLE mode)
I (1443) gateway::ble_at: [ble_at] controller config magic=0x5a5aa5a5, version=0x02509280
I (1452) BLE_INIT: BT controller compile version [b7de11e]
I (1458) BLE_INIT: Using main XTAL as clock source
I (1465) BLE_INIT: Feature Config, ADV:1, BLE_50:1, DTM:1, SCAN:1, CCA:0, SMP:1, CONNECT:1
I (1474) BLE_INIT: Bluetooth MAC: fc:01:2c:05:64:8a
I (1480) phy_init: phy_version 711,97bcf0a2,Aug 25 2025,19:04:10
W (1485) phy_init: failed to load RF calibration data (0x1102), falling back to full calibration
I (1529) phy_init: Saving new calibration data due to checksum failure or outdated calibration data, mode(2)
I (1585) gateway::ble_at: [ble_at] BLE device name set to 'Mesh'
I (1586) gateway::ble_at: [ble_at] BLE MAC = FC:01:2C:05:64:8A
I (1588) gateway::ble_at: [ble_at] ETH MAC = FC:01:2C:05:64:8B
I (1595) gateway::ble_at: [ble_at] local MTU set to 500
I (1601) gateway::ble_at: [ble_at] GATT app registered, gatts_if=3
I (1607) gateway::ble_at: [ble_at] REG_EVT status=0, app_id=1
I (1614) gateway::ble_at: [ble_at] calling create_attr_tab(gatts_if=3, n_attr=4)
I (1622) gateway::ble_at: [ble_at] create_attr_tab returned 0
I (1630) gateway::ble_at: [ble_at] GOT CREAT_ATTR_TAB_EVT
I (1634) gateway::ble_at: [ble_at] CREAT_ATTR_TAB_EVT status=0 num_handle=4 handles=0x3fcaeb5a
0x3fcaeb5a - btc_creat_tab_env
    at ??:??
I (1643) gateway::ble_at: [ble_at] attr_tab created (handles=4, svc=40, status=0)
I (1652) gateway::ble_at: [ble_at] about to call start_service(handle=40)
I (1659) gateway::ble_at: [ble_at] GATT service started (uuid=4fafc201-1fb5-459e-8fcc-c5c9c331914b)
I (1669) gateway::ble_at: [ble_at] config_adv_data queued (name + UUID + flags)
I (1680) gateway::ble_at: [ble_at] ADV_DATA configured; starting advertising...
I (1688) gateway::ble_at: [ble_at] BLE advertising active (svc=4fafc201-1fb5-459e-8fcc-c5c9c331914b)
I (1695) gateway::ble_at: [ble_at] service startup queued (GATT callback registered, AT parser ready)
I (1704) gateway::ble_at: [ble_at] notification sender runs in main loop (no dedicated thread)
I (1714) gateway: [main] starting io scan task...
I (1720) gateway::io::di: [di] scan task registered in main_loop (period=20ms, channels=16 version F16)
I (1729) gateway::io::do_: [do] output task registered in main_loop (fallback=2000 ticks version F16)
I (1739) gateway: [main] starting ai/ao task...
I (1744) gateway::channel::ao: [ao] output task registered in main_loop (period=100ms)
I (1753) gateway::channel::ai: [ai] sample task registered in main_loop (period=100ms)
I (1762) gateway::channel::calib: [adc-calib] AI0 开始 8 秒校准 (SENSOR_MIN=605 SENSOR_MAX=3016)
I (3383) gateway::device::holding_store: [holding] legacy NVS blobs retired after raw snapshot verification
I (3383) gateway::device::holding_store: [holding] persisted 2048 words to raw slot 0 generation 1
I (9820) gateway::channel::calib: [adc-calib] AI0: raw_min=2939 raw_max=2941 samples=157
W (9820) gateway::channel::calib: [adc-calib] AI0 min=2939 不在有效范围 (505..805), 跳过 min
I (9828) gateway::channel::calib: [adc-calib] AI0 校准完成: min=0 max=2941 → 保持寄存器 [2280,2288]
I (9841) gateway::udp_multicast: [udp-mcast] join group=239.0.0.1
I (9846) gateway::udp_multicast: [udp-mcast] joined via INADDR_ANY
I (9852) gateway::udp_multicast: [udp-mcast] receiver thread spawned
I (9859) gateway::udp_multicast: [udp-mcast] listening on port 5003, group 239.0.0.1
I (9867) gateway::nfc: [nfc] background thread spawned (5s polling, bounded retry backoff)
I (9929) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (9930) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
W (9987) gateway::udp_multicast: [udp-mcast] source IP filter is unconfigured; dropping all packets
I (9990) gateway::web: [http] web server started on port 80
I (9996) gateway::web: [http] listening on 0.0.0.0:80
I (10002) gateway: [main] registering modbus rtu...
I (10008) gateway::protocol: [protocol] registered: modbus-rtu
I (10014) gateway: [main] registering modbus tcp...
I (10020) gateway::protocol: [protocol] registered: modbus-tcp
W (10029) gateway::modbus::rtu_runtime: [modbus-rtu] RS485-3 runtime is implemented but disabled: UART0 is the active console
I (10038) gateway::protocol: [protocol] modbus-rtu started
I (10045) gateway::rs485::port: [rs485] opened uart1 @9600 bps, tx=45, rx=46, de=7
I (10052) gateway::modbus::rtu_runtime: [modbus-rtu] RS485-1 switched to Master, addr=1, 9600bps
I (10062) gateway::modbus::tcp_server: [mb-tcp] bound :502
I (10068) gateway::rs485::port: [rs485] opened uart2 @9600 bps, tx=42, rx=41, de=8
I (10075) gateway::modbus::rtu_runtime: [modbus-rtu] RS485-2 switched to Slave, addr=1, 9600bps
I (10085) gateway::modbus::tcp_server: [mb-tcp] bound :503
I (10091) gateway::device::holding_store: [holding] persisted 2048 words to raw slot 1 generation 2
I (10101) gateway::modbus::tcp_server: [mb-tcp] bound :504
I (10108) gateway::modbus::tcp_server: [mb-tcp] bound :5002
I (10113) gateway::modbus::tcp_server: [mb-tcp] 4 ports, max 8 clients, main-loop polling
I (10122) gateway::protocol: [protocol] modbus-tcp started
I (10129) gateway::control_logic: [control] MCA table loaded: 0 IO records, 0 analog channels
I (10137) gateway: [main] entering main loop (period=5ms)
I (10143) gateway::health: === Task-Core Assignment (7 tasks) ===
I (10150) gateway::health:   [ 0] device-store (max_stall=3)
I (10156) gateway::health:   [ 1] udp-mcast (max_stall=30)
I (10163) gateway::health:   [ 2] nfc-st25 (max_stall=30)
I (10169) gateway::health:   [ 3] http-srv (max_stall=30)
I (10175) gateway::health:   [ 4] mb-rtu-port0 (max_stall=30)
I (10181) gateway::health:   [ 5] mb-rtu-port1 (max_stall=30)
I (10188) gateway::health:   [ 6] main (max_stall=3)
I (10199) gateway::ble_at: [ble_at] physical BT LED off
I (14988) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (14991) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (19992) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (19993) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (24995) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (24996) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (29998) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (30000) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (35002) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (35003) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (40005) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (40006) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (45008) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (45011) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (50012) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (50013) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (55015) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (55016) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (60020) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (60020) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (65023) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (65025) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (70027) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (70028) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (70191) gateway: [main] uptime=59s heap=2044KB min=2035KB internal=53KB internal_min=44KB tick=12000
I (70191) gateway::health: [stack-detail] task=device-store size=12288B min_free=8672B used=29%
I (70199) gateway::health: [stack-detail] task=udp-mcast size=4096B min_free=2260B used=44%
I (70208) gateway::health: [stack-detail] task=nfc-st25 size=6144B min_free=2440B used=60%
I (70217) gateway::health: [stack-detail] task=http-srv size=10240B min_free=8552B used=16%
I (70226) gateway::health: [stack-detail] task=mb-rtu-port0 size=8192B min_free=1956B used=76%
I (70235) gateway::health: [stack-detail] task=mb-rtu-port1 size=8192B min_free=2176B used=73%
I (70245) gateway::health: [stack-detail] task=main size=32768B min_free=18812B used=42%
I (70253) gateway::health: [stack] tasks=7 low=0 min_free=1956B min_task=mb-rtu-port0 max_used=76% max_task=mb-rtu-port0
I (70265) gateway: [perf] main_loop period=5ms max_work=76441us deadline_miss=1
I (70273) gateway: [perf] phase_peak_us tcp=1092 ai_ao=914 di=1284 do=347 persist=280 eth=445 ble=713 events=252 housekeeping=74931
I (75032) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (75033) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (80035) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (80037) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (85039) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (85040) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (90044) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (90045) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (95047) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (95048) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (97408) esp_netif_handlers: eth ip: 192.168.51.226, mask: 255.255.255.0, gw: 192.168.51.1
I (97412) gateway: [eth] main loop: IP assigned 192.168.51.226
I (100050) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (100052) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (105054) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (105055) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (110058) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (110059) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (115062) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (115063) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (120066) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (120067) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (122982) gateway::web: [http] IO control: addr=0 value=true
I (124966) gateway::web: [http] IO control: addr=1 value=true
I (125071) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (125071) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (125992) gateway::web: [http] IO control: addr=2 value=true
I (130075) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (130075) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (130311) gateway: [main] uptime=120s heap=2040KB min=2020KB internal=53KB internal_min=44KB tick=24000
I (130312) gateway::health: [stack-detail] task=device-store size=12288B min_free=8672B used=29%
I (130320) gateway::health: [stack-detail] task=udp-mcast size=4096B min_free=2260B used=44%
I (130329) gateway::health: [stack-detail] task=nfc-st25 size=6144B min_free=2264B used=63%
I (130338) gateway::health: [stack-detail] task=http-srv size=10240B min_free=2504B used=75%
I (130347) gateway::health: [stack-detail] task=mb-rtu-port0 size=8192B min_free=1940B used=76%
I (130357) gateway::health: [stack-detail] task=mb-rtu-port1 size=8192B min_free=2176B used=73%
I (130366) gateway::health: [stack-detail] task=main size=32768B min_free=18812B used=42%
I (130375) gateway::health: [stack] tasks=7 low=0 min_free=1940B min_task=mb-rtu-port0 max_used=76% max_task=mb-rtu-port0
I (130387) gateway: [perf] main_loop period=5ms max_work=77910us deadline_miss=7
I (130395) gateway: [perf] phase_peak_us tcp=4156 ai_ao=733 di=3104 do=2186 persist=6465 eth=405 ble=1713 events=1540 housekeeping=76087
I (135079) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (135079) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (140082) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (140085) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (145086) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (145087) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (150090) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (150091) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (155094) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (155095) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (160099) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (160100) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (165102) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (165105) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (170106) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (170107) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (175110) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (175111) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (180114) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (180115) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (185119) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (185119) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (190122) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (190125) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (190405) gateway: [main] uptime=180s heap=2040KB min=2015KB internal=53KB internal_min=44KB tick=36000
I (190406) gateway::health: [stack-detail] task=device-store size=12288B min_free=8672B used=29%
I (190414) gateway::health: [stack-detail] task=udp-mcast size=4096B min_free=2260B used=44%
I (190423) gateway::health: [stack-detail] task=nfc-st25 size=6144B min_free=2264B used=63%
I (190432) gateway::health: [stack-detail] task=http-srv size=10240B min_free=2504B used=75%
I (190441) gateway::health: [stack-detail] task=mb-rtu-port0 size=8192B min_free=1940B used=76%
I (190450) gateway::health: [stack-detail] task=mb-rtu-port1 size=8192B min_free=2176B used=73%
I (190460) gateway::health: [stack-detail] task=main size=32768B min_free=18812B used=42%
I (190469) gateway::health: [stack] tasks=7 low=0 min_free=1940B min_task=mb-rtu-port0 max_used=76% max_task=mb-rtu-port0
I (190480) gateway: [perf] main_loop period=5ms max_work=77630us deadline_miss=1
I (190488) gateway: [perf] phase_peak_us tcp=1095 ai_ao=805 di=1810 do=88 persist=96 eth=428 ble=236 events=72 housekeeping=76064
I (195127) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (195128) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (200130) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (200132) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (205134) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (205135) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (210138) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (210139) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (215142) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (215143) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (220147) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (220147) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (225150) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (225152) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (230154) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (230155) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (235158) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (235159) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (236223) gateway::web: [http] IO control: addr=3 value=true
I (237346) gateway::web: [http] IO control: addr=4 value=true
I (238178) gateway::web: [http] IO control: addr=5 value=true
I (239057) gateway::web: [http] IO control: addr=6 value=true
I (239968) gateway::web: [http] IO control: addr=7 value=true
I (240162) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (240163) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (241096) gateway::web: [http] IO control: addr=8 value=true
I (241947) gateway::web: [http] IO control: addr=9 value=true
I (242828) gateway::web: [http] IO control: addr=10 value=true
I (243977) gateway::web: [http] IO control: addr=11 value=true
I (245166) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (245167) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (245789) gateway::web: [http] IO control: addr=12 value=true
I (248263) gateway::web: [http] IO control: addr=13 value=true
I (249455) gateway::web: [http] IO control: addr=14 value=true
I (250170) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (250171) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (250526) gateway: [main] uptime=240s heap=2040KB min=2012KB internal=53KB internal_min=44KB tick=48000
I (250527) gateway::health: [stack-detail] task=device-store size=12288B min_free=8672B used=29%
I (250535) gateway::health: [stack-detail] task=udp-mcast size=4096B min_free=2260B used=44%
I (250544) gateway::health: [stack-detail] task=nfc-st25 size=6144B min_free=2264B used=63%
I (250553) gateway::health: [stack-detail] task=http-srv size=10240B min_free=2504B used=75%
I (250562) gateway::health: [stack-detail] task=mb-rtu-port0 size=8192B min_free=1940B used=76%
I (250571) gateway::health: [stack-detail] task=mb-rtu-port1 size=8192B min_free=2176B used=73%
I (250581) gateway::health: [stack-detail] task=main size=32768B min_free=18812B used=42%
I (250590) gateway::health: [stack] tasks=7 low=0 min_free=1940B min_task=mb-rtu-port0 max_used=76% max_task=mb-rtu-port0
I (250601) gateway: [perf] main_loop period=5ms max_work=77601us deadline_miss=8
I (250609) gateway: [perf] phase_peak_us tcp=1302 ai_ao=830 di=2057 do=1548 persist=7292 eth=425 ble=252 events=72 housekeeping=76077
I (250801) gateway::web: [http] IO control: addr=15 value=true
I (255174) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (255175) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (260178) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (260179) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (265182) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (265183) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (270187) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (270187) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (275190) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (275192) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (280194) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (280195) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (285198) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (285199) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (290202) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (290203) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (295207) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (295208) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (300210) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (300212) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (305214) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (305215) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (310218) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (310219) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (310623) gateway: [main] uptime=300s heap=2040KB min=2012KB internal=53KB internal_min=44KB tick=60000
I (310624) gateway::health: [stack-detail] task=device-store size=12288B min_free=8672B used=29%
I (310632) gateway::health: [stack-detail] task=udp-mcast size=4096B min_free=2260B used=44%
I (310641) gateway::health: [stack-detail] task=nfc-st25 size=6144B min_free=2264B used=63%
I (310650) gateway::health: [stack-detail] task=http-srv size=10240B min_free=2504B used=75%
I (310659) gateway::health: [stack-detail] task=mb-rtu-port0 size=8192B min_free=1940B used=76%
I (310668) gateway::health: [stack-detail] task=mb-rtu-port1 size=8192B min_free=2176B used=73%
I (310678) gateway::health: [stack-detail] task=main size=32768B min_free=18812B used=42%
I (310687) gateway::health: [stack] tasks=7 low=0 min_free=1940B min_task=mb-rtu-port0 max_used=76% max_task=mb-rtu-port0
I (310698) gateway: [perf] main_loop period=5ms max_work=77630us deadline_miss=2
I (310706) gateway: [perf] phase_peak_us tcp=1824 ai_ao=837 di=2078 do=1334 persist=4929 eth=426 ble=225 events=137 housekeeping=76078
I (315222) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (315223) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (320227) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (320227) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (325231) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (325231) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (330234) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (330237) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (335238) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (335239) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (340242) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (340243) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (345246) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (345247) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (350251) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (350252) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (355254) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (355256) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (360258) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (360259) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (362296) gateway::ble_at: [ble_at] BLE frame: tx_id=0x0001 proto=0x0000 len=4 data_len=4
I (362297) gateway::ble_at: [ble_at] DI status report: di_bits=0x000000000000000F bytes=2
I (365262) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (365263) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36























[2026-10-28T03:24:42Z INFO ] Using flash stub
Chip type:         esp32s3 (revision v0.2)
Crystal frequency: 40 MHz
Flash size:        8MB
Features:          WiFi, BLE, Embedded Flash
MAC address:       fc:01:2c:05:64:b8
[2026-10-28T03:24:42Z INFO ] Erasing otadata (Data(Ota))...
App/part. size:    1,744,704/2,359,296 bytes, 73.95%
[00:00:01] [========================================]       1/1       0x0      Verifying... OK!
[00:00:00] [========================================]       1/1       0x8000   Verifying... OK!
[00:01:44] [========================================]      59/59      0x20000  Verifying... OK![2026-10-28T03:26:29Z INFO ] Flashing has completed!
Commands:
    CTRL+R    Reset chip
    CTRL+C    Exit

ESP-ROM:esp32s3-20210327
Build:Mar 27 2021
rst:0x1 (POWERON),boot:0x2b (SPI_FAST_FLASH_BOOT)
SPIWP:0xee
mode:DIO, clock div:2
load:0x3fce2820,len:0x14f0
load:0x403c8700,len:0xd24
load:0x403cb700,len:0x2f48
entry 0x403c8924
I (29) boot: ESP-IDF v5.5.4 2nd stage bootloader
I (29) boot: compile time Oct 14 2026 23:48:56
I (30) boot: Multicore bootloader
I (30) boot: chip revision: v0.2
I (33) boot: efuse block revision: v1.3
I (36) boot.esp32s3: Boot SPI Speed : 40MHz
I (40) boot.esp32s3: SPI Mode       : DIO
I (44) boot.esp32s3: SPI Flash Size : 8MB
I (48) boot: Enabling RNG early entropy source...
I (52) boot: Partition Table:
I (55) boot: ## Label            Usage          Type ST Offset   Length
I (61) boot:  0 nvs              WiFi data        01 02 00009000 00006000
I (67) boot:  1 phy_init         RF data          01 01 0000f000 00001000
I (74) boot:  2 otadata          OTA data         01 00 00010000 00002000
I (81) boot:  3 nvs_keys         NVS keys         01 04 00017000 00001000
I (87) boot:  4 holding          Unknown data     01 06 00018000 00008000
I (94) boot:  5 factory          factory app      00 00 00020000 00240000
I (100) boot:  6 ota_0            OTA app          00 10 00260000 00240000
I (107) boot:  7 ota_1            OTA app          00 11 004a0000 00240000
I (113) boot:  8 coredump         Unknown data     01 03 006e0000 00010000
I (120) boot:  9 ble_mesh         WiFi data        01 02 006f0000 00010000
I (126) boot: 10 storage          Unknown data     01 81 00700000 00100000
I (133) boot: End of partition table
I (136) boot: Defaulting to factory image
I (140) esp_image: segment 0: paddr=00020020 vaddr=3c130020 size=5eaech (387820) map
I (243) esp_image: segment 1: paddr=0007eb14 vaddr=3fca4000 size=01504h (  5380) load
I (245) esp_image: segment 2: paddr=00080020 vaddr=42000020 size=12875ch (1214300) map
I (545) esp_image: segment 3: paddr=001a8784 vaddr=3fca5504 size=057b8h ( 22456) load
I (551) esp_image: segment 4: paddr=001adf44 vaddr=40378000 size=1bfach (114604) load
I (584) esp_image: segment 5: paddr=001c9ef8 vaddr=50000000 size=00020h (    32) load
I (595) boot: Loaded app from partition at offset 0x20000
I (596) boot: Disabling RNG early entropy source...
I (606) esp_psram: Found 2MB PSRAM device
I (606) esp_psram: Speed: 80MHz
I (607) cpu_start: Multicore app
I (801) esp_psram: SPI SRAM memory test OK
I (810) cpu_start: GPIO 44 and 43 are used as console UART I/O pins
I (811) cpu_start: Pro cpu start user code
I (811) cpu_start: cpu freq: 240000000 Hz
I (815) app_init: Application information:
I (819) app_init: Project name:     libespidf
I (824) app_init: App version:      2.2.7
I (829) app_init: Compile time:     Oct 14 2026 23:48:17
I (835) app_init: ELF file SHA256:  a2143d798...
I (840) app_init: ESP-IDF:          v5.5.4
I (845) efuse_init: Min chip rev:     v0.0
I (850) efuse_init: Max chip rev:     v0.99
I (855) efuse_init: Chip rev:         v0.2
I (860) heap_init: Initializing. RAM available for dynamic allocation:
I (867) heap_init: At 3FCB1A60 len 00037CB0 (223 KiB): RAM
I (873) heap_init: At 3FCE9710 len 00005724 (21 KiB): RAM
I (879) heap_init: At 3FCF0000 len 00008000 (32 KiB): DRAM
I (885) heap_init: At 600FE000 len 00001FE8 (7 KiB): RTCRAM
I (892) esp_psram: Adding pool of 2048K of PSRAM memory to heap allocator
I (901) spi_flash: detected chip: winbond
I (904) spi_flash: flash io: dio
I (909) sleep_gpio: Configure to isolate all GPIO pins in sleep state
I (915) sleep_gpio: Enable automatic switching of GPIO sleep configuration
I (923) esp_core_dump_flash: Init core dump to flash
I (929) esp_core_dump_flash: Found partition 'coredump' @ 6e0000 65536 bytes
D (936) esp_core_dump_flash: Blank core dump partition!
I (941) coexist: coex firmware version: 093e3d2
I (946) coexist: coexist rom version e7ae62f
I (952) main_task: Started on CPU0
I (956) esp_psram: Reserving pool of 64K of internal memory for DMA/internal allocations
I (965) main_task: Calling app_main()
I (970) gateway: ================================================
I (975) gateway: esp32s3-iot-gateway v2.2.7
I (980) gateway: [build] id=fd89b4ca1ec9
I (985) gateway: ESP32-S3R2 IoT Gateway starting...
I (990) gateway: ================================================
I (1001) gateway::hal::adc::adc_continuous: [adc] Continuous+DMA mode started: 6 ch, 1kHz total, Type2 12-bit
I (1060) gateway::hal: [hal] IO/LED board power enabled (gpio21 + gpio33 HIGH)
I (1061) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (1064) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (1072) gateway::hal::pca9555: [pca9555] io bus: DI@0x40(input) DO@0x42(output)
I (1079) esp_idf_svc::eventloop: System event loop dropped
Error: Io("I2C 0x40 wr 0x06: NACK addr")
I (1088) main_task: Returned from app_main()






[2026-10-28T03:27:56Z INFO ] Using flash stub
Chip type:         esp32s3 (revision v0.2)
Crystal frequency: 40 MHz
Flash size:        8MB
Features:          WiFi, BLE, Embedded Flash
MAC address:       fc:01:2c:05:64:b8
[2026-10-28T03:27:57Z INFO ] Erasing otadata (Data(Ota))...
App/part. size:    1,744,704/2,359,296 bytes, 73.95%
[00:00:01] [========================================]       1/1       0x0      Verifying... OK!
[00:00:00] [========================================]       1/1       0x8000   Verifying... OK!
[00:01:44] [========================================]      59/59      0x20000  Verifying... OK!                               [2026-10-28T03:29:44Z INFO ] Flashing has completed!
Commands:
    CTRL+R    Reset chip
    CTRL+C    Exit

ESP-ROM:esp32s3-20210327
Build:Mar 27 2021
rst:0x1 (POWERON),boot:0x2b (SPI_FAST_FLASH_BOOT)
SPIWP:0xee
mode:DIO, clock div:2
load:0x3fce2820,len:0x14f0
load:0x403c8700,len:0xd24
load:0x403cb700,len:0x2f48
entry 0x403c8924
I (29) boot: ESP-IDF v5.5.4 2nd stage bootloader
I (29) boot: compile time Oct 14 2026 23:48:56
I (30) boot: Multicore bootloader
I (30) boot: chip revision: v0.2
I (33) boot: efuse block revision: v1.3
I (36) boot.esp32s3: Boot SPI Speed : 40MHz
I (40) boot.esp32s3: SPI Mode       : DIO
I (44) boot.esp32s3: SPI Flash Size : 8MB
I (48) boot: Enabling RNG early entropy source...
I (52) boot: Partition Table:
I (55) boot: ## Label            Usage          Type ST Offset   Length
I (61) boot:  0 nvs              WiFi data        01 02 00009000 00006000
I (67) boot:  1 phy_init         RF data          01 01 0000f000 00001000
I (74) boot:  2 otadata          OTA data         01 00 00010000 00002000
I (81) boot:  3 nvs_keys         NVS keys         01 04 00017000 00001000
I (87) boot:  4 holding          Unknown data     01 06 00018000 00008000
I (94) boot:  5 factory          factory app      00 00 00020000 00240000
I (100) boot:  6 ota_0            OTA app          00 10 00260000 00240000
I (107) boot:  7 ota_1            OTA app          00 11 004a0000 00240000
I (113) boot:  8 coredump         Unknown data     01 03 006e0000 00010000
I (120) boot:  9 ble_mesh         WiFi data        01 02 006f0000 00010000
I (126) boot: 10 storage          Unknown data     01 81 00700000 00100000
I (133) boot: End of partition table
I (136) boot: Defaulting to factory image
I (140) esp_image: segment 0: paddr=00020020 vaddr=3c130020 size=5eaech (387820) map
I (243) esp_image: segment 1: paddr=0007eb14 vaddr=3fca4000 size=01504h (  5380) load
I (245) esp_image: segment 2: paddr=00080020 vaddr=42000020 size=12875ch (1214300) map
I (545) esp_image: segment 3: paddr=001a8784 vaddr=3fca5504 size=057b8h ( 22456) load
I (551) esp_image: segment 4: paddr=001adf44 vaddr=40378000 size=1bfach (114604) load
I (584) esp_image: segment 5: paddr=001c9ef8 vaddr=50000000 size=00020h (    32) load
I (595) boot: Loaded app from partition at offset 0x20000
I (596) boot: Disabling RNG early entropy source...
I (606) esp_psram: Found 2MB PSRAM device
I (606) esp_psram: Speed: 80MHz
I (607) cpu_start: Multicore app
I (801) esp_psram: SPI SRAM memory test OK
I (810) cpu_start: GPIO 44 and 43 are used as console UART I/O pins
I (811) cpu_start: Pro cpu start user code
I (811) cpu_start: cpu freq: 240000000 Hz
I (815) app_init: Application information:
I (819) app_init: Project name:     libespidf
I (824) app_init: App version:      2.2.7
I (829) app_init: Compile time:     Oct 14 2026 23:48:17
I (835) app_init: ELF file SHA256:  a2143d798...
I (840) app_init: ESP-IDF:          v5.5.4
I (845) efuse_init: Min chip rev:     v0.0
I (850) efuse_init: Max chip rev:     v0.99
I (855) efuse_init: Chip rev:         v0.2
I (860) heap_init: Initializing. RAM available for dynamic allocation:
I (867) heap_init: At 3FCB1A60 len 00037CB0 (223 KiB): RAM
I (873) heap_init: At 3FCE9710 len 00005724 (21 KiB): RAM
I (879) heap_init: At 3FCF0000 len 00008000 (32 KiB): DRAM
I (885) heap_init: At 600FE000 len 00001FE8 (7 KiB): RTCRAM
I (892) esp_psram: Adding pool of 2048K of PSRAM memory to heap allocator
I (901) spi_flash: detected chip: winbond
I (904) spi_flash: flash io: dio
I (909) sleep_gpio: Configure to isolate all GPIO pins in sleep state
I (915) sleep_gpio: Enable automatic switching of GPIO sleep configuration
I (923) esp_core_dump_flash: Init core dump to flash
I (929) esp_core_dump_flash: Found partition 'coredump' @ 6e0000 65536 bytes
D (936) esp_core_dump_flash: Blank core dump partition!
I (941) coexist: coex firmware version: 093e3d2
I (946) coexist: coexist rom version e7ae62f
I (952) main_task: Started on CPU0
I (956) esp_psram: Reserving pool of 64K of internal memory for DMA/internal allocations
I (965) main_task: Calling app_main()
I (970) gateway: ================================================
I (975) gateway: esp32s3-iot-gateway v2.2.7
I (980) gateway: [build] id=fd89b4ca1ec9
I (985) gateway: ESP32-S3R2 IoT Gateway starting...
I (990) gateway: ================================================
I (1001) gateway::hal::adc::adc_continuous: [adc] Continuous+DMA mode started: 6 ch, 1kHz total, Type2 12-bit
I (1060) gateway::hal: [hal] IO/LED board power enabled (gpio21 + gpio33 HIGH)
I (1061) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (1064) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (1072) gateway::hal::pca9555: [pca9555] io bus: DI@0x40(input) DO@0x42(output)
I (1083) gateway::hal::pca9555: [pca9555] status LEDs: PWR+RUN on (0xDE)
I (1085) gateway::hal::pca9555: [pca9555] led bus: all chips ready, all LEDs off except PWR/RUN
I (1095) gateway::hal::pca9555: [pca9555] F16 (16DI+16DO) init complete
I (1102) gateway: [main] initializing NVS flash...
I (1120) gateway::device: [device] NVS namespace 'gateway' opened
I (1121) gateway: [main] reset reason=1 (POWERON) previous=1 (POWERON)
I (1124) gateway::rs485::dip: [dip] AD0=0 AD1=0 AD2=0 AD3=0 → address=0 (gpio 19/20/48/47)
I (1133) gateway: [main] starting device protocol store...
I (1139) gateway::device: [device] no active flag, trying legacy single blob
I (1147) gateway::device: [device] nvs empty or all blobs corrupted, using defaults
I (1155) gateway::device: [device] dev_text blob not present, using empty
I (1162) gateway::device: [device] no valid dev_text blob, using empty
I (1170) gateway::device::system_config: [cfg] nvs empty, using defaults
I (1178) gateway::device::system_config: [cfg] eth_mac loaded from hw: FC:01:2C:05:64:BB
I (1186) gateway::device::system_config: [cfg] ble_mac loaded from hw: FC:01:2C:05:64:BA
I (1195) gateway::device::holding_store: [holding] raw partition found: offset=0x18000 size=32KB
I (1208) gateway::device::holding_store: [holding] loaded raw slot 0 generation 1 (2048 words)
I (1213) gateway::device: [device] imported legacy BLE ID from D98..D101: 'Mesh'
I (1223) gateway::device: [device] do_bits magic not found or wrong (0x0), default 0
I (1230) gateway::device: [device] DO bits loaded: 0x0000000000000000
I (1237) gateway::device: [device] proto loaded: 0 words, version=0x0000
I (1244) gateway::device: [device] cfg loaded: sn='ESP32-001' ip=192.168.51.226 mac=FC:01:2C:05:64:BB fw=0x00E3
I (1257) gateway::device: [device] DeviceActor started (mailbox consumer thread)
I (1263) gateway: [main] device init ok
I (1268) gateway: [main] reset count=2 reason=1 (POWERON)
I (1274) gateway: [main] starting ethernet (W5500)...
I (1280) gateway::ethernet::w5500: [eth] initializing W5500 over SPI2 (MISO=11,MOSI=12,SCLK=10,CS=9,INT=13,RST=14)...
I (1291) gateway::ethernet::w5500: [eth] resetting W5500 via GPIO14...
I (1350) gateway::ethernet::w5500: [eth] SPI bus initialized (host=2, DMA=auto)
I (1363) gateway::ethernet::w5500: [eth] setting MAC: FC:01:2C:05:64:BB
I (1365) esp_eth.netif.netif_glue: fc:01:2c:05:64:bb
I (1365) esp_eth.netif.netif_glue: ethernet attached to netif
I (1372) gateway::ethernet::w5500: [eth] network config: dhcp=false ip=192.168.51.226 mask=255.255.255.0 gw=192.168.51.1 dns=192.168.51.1
I (1386) gateway::ethernet::w5500: [eth] static IP configured: 192.168.51.226 / 255.255.255.0 gw 192.168.51.1 dns 192.168.51.1
I (1397) gateway::ethernet::w5500: [eth] IP/ETH event handlers registered
I (1414) gateway::ethernet::w5500: [eth] W5500 driver installed and started, eth_handle=0x3fcd0c0c
I (1415) gateway::ethernet::w5500: [eth] heartbeat registered in main_loop (period=5s)
I (1422) gateway::ethernet::w5500: [eth] W5500 startup complete
I (1429) gateway: [main] starting ble gatt server...
I (1435) gateway::ble_at: [ble_at] service starting (uuid=4fafc201-1fb5-459e-8fcc-c5c9c331914b)
I (1444) gateway::ble_at: [ble_at] initializing BT controller (BLE mode)
I (1451) gateway::ble_at: [ble_at] controller config magic=0x5a5aa5a5, version=0x02509280
I (1460) BLE_INIT: BT controller compile version [b7de11e]
I (1466) BLE_INIT: Using main XTAL as clock source
I (1473) BLE_INIT: Feature Config, ADV:1, BLE_50:1, DTM:1, SCAN:1, CCA:0, SMP:1, CONNECT:1
I (1482) BLE_INIT: Bluetooth MAC: fc:01:2c:05:64:ba
I (1488) phy_init: phy_version 711,97bcf0a2,Aug 25 2025,19:04:10
I (1560) gateway::ble_at: [ble_at] BLE device name set to 'Mesh'
I (1561) gateway::ble_at: [ble_at] BLE MAC = FC:01:2C:05:64:BA
I (1563) gateway::ble_at: [ble_at] ETH MAC = FC:01:2C:05:64:BB
I (1569) gateway::ble_at: [ble_at] local MTU set to 500
I (1576) gateway::ble_at: [ble_at] GATT app registered, gatts_if=3
I (1582) gateway::ble_at: [ble_at] REG_EVT status=0, app_id=1
I (1588) gateway::ble_at: [ble_at] calling create_attr_tab(gatts_if=3, n_attr=4)
I (1597) gateway::ble_at: [ble_at] create_attr_tab returned 0
I (1605) gateway::ble_at: [ble_at] GOT CREAT_ATTR_TAB_EVT
I (1609) gateway::ble_at: [ble_at] CREAT_ATTR_TAB_EVT status=0 num_handle=4 handles=0x3fcaeb5a
0x3fcaeb5a - btc_creat_tab_env
    at ??:??
I (1618) gateway::ble_at: [ble_at] attr_tab created (handles=4, svc=40, status=0)
I (1626) gateway::ble_at: [ble_at] about to call start_service(handle=40)
I (1634) gateway::ble_at: [ble_at] GATT service started (uuid=4fafc201-1fb5-459e-8fcc-c5c9c331914b)
I (1644) gateway::ble_at: [ble_at] config_adv_data queued (name + UUID + flags)
I (1655) gateway::ble_at: [ble_at] ADV_DATA configured; starting advertising...
I (1663) gateway::ble_at: [ble_at] BLE advertising active (svc=4fafc201-1fb5-459e-8fcc-c5c9c331914b)
I (1670) gateway::ble_at: [ble_at] service startup queued (GATT callback registered, AT parser ready)
I (1679) gateway::ble_at: [ble_at] notification sender runs in main loop (no dedicated thread)
I (1689) gateway: [main] starting io scan task...
I (1694) gateway::io::di: [di] scan task registered in main_loop (period=20ms, channels=16 version F16)
I (1704) gateway::io::do_: [do] output task registered in main_loop (fallback=2000 ticks version F16)
I (1714) gateway: [main] starting ai/ao task...
I (1719) gateway::channel::ao: [ao] output task registered in main_loop (period=100ms)
I (1728) gateway::channel::ai: [ai] sample task registered in main_loop (period=100ms)
I (1737) gateway::channel::calib: [adc-calib] AI0 开始 8 秒校准 (SENSOR_MIN=605 SENSOR_MAX=3016)
I (9752) gateway::channel::calib: [adc-calib] AI0: raw_min=2848 raw_max=2851 samples=157
W (9752) gateway::channel::calib: [adc-calib] AI0 min=2848 不在有效范围 (505..805), 跳过 min
W (9760) gateway::channel::calib: [adc-calib] AI0 max=2851 不在有效范围 (2916..3216), 跳过 max
W (9770) gateway::channel::calib: [adc-calib] AI0 校准跳过 (min/max 均超出范围)
I (9781) gateway::udp_multicast: [udp-mcast] receiver thread spawned
I (9785) gateway::udp_multicast: [udp-mcast] join group=239.0.0.1
I (9792) gateway::nfc: [nfc] background thread spawned (5s polling, bounded retry backoff)
I (9801) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (9808) gateway::udp_multicast: [udp-mcast] joined via INADDR_ANY
I (9814) gateway::udp_multicast: [udp-mcast] listening on port 5003, group 239.0.0.1
I (9823) gateway::web: [http] listening on 0.0.0.0:80
I (9828) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (9835) gateway::web: [http] web server started on port 80
W (9841) gateway::udp_multicast: [udp-mcast] source IP filter is unconfigured; dropping all packets
I (9851) gateway: [main] registering modbus rtu...
I (9856) gateway::protocol: [protocol] registered: modbus-rtu
I (9863) gateway: [main] registering modbus tcp...
I (9868) gateway::protocol: [protocol] registered: modbus-tcp
W (9877) gateway::modbus::rtu_runtime: [modbus-rtu] RS485-3 runtime is implemented but disabled: UART0 is the active console
I (9886) gateway::protocol: [protocol] modbus-rtu started
I (9893) gateway::rs485::port: [rs485] opened uart2 @9600 bps, tx=42, rx=41, de=8
I (9901) gateway::rs485::port: [rs485] opened uart1 @9600 bps, tx=45, rx=46, de=7
I (9909) gateway::modbus::rtu_runtime: [modbus-rtu] RS485-1 switched to Master, addr=1, 9600bps
I (9918) gateway::modbus::rtu_runtime: [modbus-rtu] RS485-2 switched to Slave, addr=1, 9600bps
I (9928) gateway::modbus::tcp_server: [mb-tcp] bound :502
I (9934) gateway::modbus::tcp_server: [mb-tcp] bound :503
I (9940) gateway::modbus::tcp_server: [mb-tcp] bound :504
I (9946) gateway::modbus::tcp_server: [mb-tcp] bound :5002
I (9952) gateway::modbus::tcp_server: [mb-tcp] 4 ports, max 8 clients, main-loop polling
I (9960) gateway::protocol: [protocol] modbus-tcp started
I (9967) gateway::control_logic: [control] MCA table loaded: 0 IO records, 0 analog channels
I (9976) gateway: [main] entering main loop (period=5ms)
I (9982) gateway::health: === Task-Core Assignment (7 tasks) ===
I (9988) gateway::health:   [ 0] device-store (max_stall=3)
I (9995) gateway::health:   [ 1] udp-mcast (max_stall=30)
I (10001) gateway::health:   [ 2] nfc-st25 (max_stall=30)
I (10007) gateway::health:   [ 3] http-srv (max_stall=30)
I (10013) gateway::health:   [ 4] mb-rtu-port0 (max_stall=30)
I (10019) gateway::health:   [ 5] mb-rtu-port1 (max_stall=30)
I (10026) gateway::health:   [ 6] main (max_stall=3)
I (10037) gateway::ble_at: [ble_at] physical BT LED off
I (14836) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (14838) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (19840) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (19841) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (24844) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (24845) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (29848) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (29849) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (34852) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (34853) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (39855) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (39856) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (44859) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (44860) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (49863) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (49864) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (54866) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (54869) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (59870) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (59871) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (64874) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (64875) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (69878) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (69878) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (70029) gateway: [main] uptime=59s heap=2044KB min=2038KB internal=53KB internal_min=52KB tick=12000
I (70029) gateway::health: [stack-detail] task=device-store size=12288B min_free=9688B used=21%
I (70037) gateway::health: [stack-detail] task=udp-mcast size=4096B min_free=2260B used=44%
I (70046) gateway::health: [stack-detail] task=nfc-st25 size=6144B min_free=2468B used=59%
I (70055) gateway::health: [stack-detail] task=http-srv size=10240B min_free=8548B used=16%
I (70064) gateway::health: [stack-detail] task=mb-rtu-port0 size=8192B min_free=1952B used=76%
I (70073) gateway::health: [stack-detail] task=mb-rtu-port1 size=8192B min_free=2172B used=73%
I (70083) gateway::health: [stack-detail] task=main size=32768B min_free=18812B used=42%
I (70091) gateway::health: [stack] tasks=7 low=0 min_free=1952B min_task=mb-rtu-port0 max_used=76% max_task=mb-rtu-port0
I (70103) gateway: [perf] main_loop period=5ms max_work=76458us deadline_miss=1
I (70111) gateway: [perf] phase_peak_us tcp=1012 ai_ao=1077 di=1087 do=81 persist=90 eth=365 ble=721 events=104 housekeeping=74960
I (74881) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (74882) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (79884) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (79886) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (84888) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (84890) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
I (89892) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37
I (89893) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36
