# GitHub CI 与发布

工程使用两类 GitHub Actions：

- `ci.yml`：PR 执行主机回归；推送提交执行主机回归和 DEFAULT/F3/F4 固件 Clippy；推送 `main` 后自动生成三种生产包并保存 14 天。
- `release.yml`：推送 `vX.Y.Z` 标签后，在 self-hosted ESP32 runner 上执行完整检查、三种生产构建、SHA-256 校验和 GitHub Release 发布。

## Self-hosted runner

固件目标是 `xtensa-esp32s3-espidf`，依赖 ESP-IDF v5.5.4、ESP Rust 工具链、`ldproxy`、`espflash` 和 Python `esptool`。固件 job 使用标签：`self-hosted, esp32s3`。

Runner 必须预装并验证：

```bash
rustup toolchain list
cargo --version
espflash --version
python3 -m esptool version
test -d /Users/takumi/Workspace/esp-idf
```

仓库的 `.cargo/config.toml` 当前固定 `IDF_PATH` 为本机路径。如果 runner 使用其他路径，应在 runner 环境中调整配置；不要把本机路径提交为 GitHub secret。

## 提交、打包、发布

普通提交和 PR 执行检测。推送到 `main` 自动生成 `release/v<version>/default`、`f3`、`f4` 三种生产包并保存为 artifact。

正式发布前必须统一 `Cargo.toml`、`Cargo.lock` 和 `sdkconfig.defaults` 的版本，然后推送匹配标签，例如 `v2.5.0`。工作流会拒绝版本不匹配的标签。

发布资产格式为 `imc-v<version>-production.tar.gz` 及其 SHA-256 文件。GitHub Actions 使用内置 `GITHUB_TOKEN`，仓库设置需允许 Actions 创建和写入 Releases。

## 硬件边界

CI 不会自动连接真实串口、不执行全擦、不刷写设备。F3/F4/DEFAULT 的真实硬件验收仍需人工明确选择型号并执行项目烧录命令。
