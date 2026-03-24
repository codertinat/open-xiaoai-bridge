# Changelog

All notable changes to this project will be documented in this file.

## v1.0.3 - 2026-03-24

### 重点更新

- 豆包 TTS 升级支持新的 2.0 音色，并补充配套的辅助脚本与接口文档，便于查询和验证可用音色。
- 新增 `scripts/clone_voice.py` 声音复刻脚本，支持提交音频样本并查询训练状态。
- 新增 `scripts/generate_tts.py` 音频生成脚本，可按指定 `speaker_id`、文本和情感参数导出音频文件。
- 新增播放服务端音频文件的能力，可通过 API 直接下发本地文件进行播放。
- 优化 OpenClaw TTS 打断与设备音频关闭流程，减少播放被打断后残留音频状态未清理的问题。

### 修复与优化

- 修复外部唤醒词触发时，小爱仍然回声式回复的问题，降低路由到第三方 AI 时的干扰。
- 修复用户喊出“小爱同学”打断后，小智唤醒会话没有完全恢复的问题，避免后续唤醒失效。
- 在 Doubao TTS API 返回成功前增加请求校验，避免无效请求被误判为成功。
- 调整部分 XiaoZhi/OpenClaw 内部流程与日志细节，减少连续对话等待和排障成本。

### 文档更新

- 补充 Doubao TTS 接口、声音复刻和指定音色导出脚本的使用说明。

### Full Changelog

- https://github.com/coderzc/open-xiaoai-bridge/compare/v1.0.2...v1.0.3
