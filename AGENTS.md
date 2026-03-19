# AGENTS.md - Bridge Module Guide

> 本项目让小爱音箱接入小智 AI 和 OpenClaw 等外部 AI 服务。
> 通过接管音箱的音频输入输出，实现与第三方 AI 服务的对话。

## 项目架构

```
open-xiaoai-bridge/
├── main.py               # 程序入口
├── config.py             # 用户配置文件（唤醒词、TTS、OpenClaw 等）
├── native/               # Rust 原生扩展源码（maturin 编译）
│   ├── src/
│   │   ├── lib.rs        # Rust Python 扩展入口（含 stop/start_recording RPC）
│   │   ├── server.rs     # 音频服务实现
│   │   ├── python.rs     # PyO3 Python 绑定
│   │   └── tts/          # TTS 音频处理（流式/非流式、PCM 直通、延迟测试接口）
│   └── Cargo.toml
├── core/                 # Python 核心源码
│   ├── app.py            # MainApp: 应用主控制器
│   ├── xiaoai.py         # XiaoAI: 小爱音箱接口（事件、TTS、控制）
│   ├── xiaoai_conversation.py # XiaoAIConversationController: 小爱连续对话策略
│   ├── xiaozhi.py        # XiaoZhi: 小智 AI WebSocket 协议
│   ├── openclaw.py       # OpenClawManager: OpenClaw 网关连接
│   ├── openclaw_conversation.py # OpenClawConversationController: OpenClaw 连续对话
│   ├── ref.py            # 全局状态管理（get/set）
│   ├── wakeup_session.py # WakeupSessionManager: 小智唤醒会话状态机
│   ├── assets/           # 静态资源
│   │   └── sounds/       # 音效文件（tts_notify.mp3 等）
│   ├── services/         # 服务层
│   │   ├── speaker.py    # SpeakerManager: 音箱控制（播放/TTS/唤醒）
│   │   ├── api_server.py # HTTP API 服务（远程控制）
│   │   ├── audio/        # 音频处理
│   │   │   ├── asr/      # 离线语音识别（SherpaASR / SenseVoice）
│   │   │   ├── kws/      # 关键词唤醒（Sherpa）
│   │   │   ├── vad/      # 语音活动检测（Silero）
│   │   │   ├── stream.py # 全局音频流管理
│   │   │   └── codec.py  # 音频编解码
│   │   ├── tts/          # TTS 服务
│   │   │   └── doubao.py # Doubao TTS 客户端
│   │   └── protocols/    # 通信协议
│   ├── models/           # 模型文件目录（.gitignore 排除）
│   │   ├── *.onnx        # KWS / VAD 模型
│   │   ├── tokens.txt    # KWS tokens
│   │   ├── keywords.txt  # 自定义唤醒词
│   │   └── sherpa-onnx-sense-voice-*/ # SenseVoice ASR 模型（需手动下载）
│   └── utils/            # 工具类
└── skills/               # AI Agent 工具技能
    └── xiaoai-tts/       # 通过 HTTP API 控制小爱音箱播放语音
```

## 系统架构

完整架构图见 [README.md 系统架构章节](README.md#系统架构)。

## 核心组件说明

### 1. MainApp (app.py)
应用主控制器，单例模式管理整个应用生命周期：
- 初始化 XiaoAI 服务（必须）
- 可选初始化 XiaoZhi（小智 AI）
- 可选初始化 OpenClaw
- 可选启动 API Server
- 管理音频设备状态（IDLE/LISTENING/SPEAKING/CONNECTING）

**边界约束**:
- `MainApp` 是业务主循环和设备状态的单一入口。
- `device_state` 以 `MainApp` 为准，其他模块通过 `XiaoZhi.set_device_state()` 代理回 `MainApp`，不要各自维护平行状态。

### 2. XiaoAI (xiaoai.py)
小爱音箱交互接口：
- `on_input_data`: 接收麦克风音频输入
- `on_output_data`: 发送音频到扬声器
- `on_event`: 处理小爱事件（语音识别结果、播放状态等）
- `SpeakerManager`: 控制播放、TTS、唤醒等

**重要**: 必须通过 `set_xiaoai(XiaoAI)` 注册，否则 `get_xiaoai()` 返回 None。

**边界约束**:
- `xiaoai.py` 负责设备接入和事件桥接，不负责承载完整的小爱连续对话策略。
- 小爱连续对话状态放在 `xiaoai_conversation.py` 中，避免和唤醒状态机混在一起。

### 3. XiaoZhi (xiaozhi.py)
小智 AI 协议客户端：
- WebSocket 连接小智服务器
- 发送音频/文本，接收 AI 响应
- 支持 VAD（语音检测）和 KWS（关键词唤醒）

**边界约束**:
- `xiaozhi.py` 只负责协议收发，不负责唤醒策略和连续对话策略。
- 协议层 `session_id` 必须由服务端消息更新，不能长期使用空值发送 `listen/abort/stop` 控制消息。

### 4. OpenClawManager (openclaw.py)
OpenClaw 网关客户端：
- WebSocket 连接到 OpenClaw Gateway
- `session_key`: 指定 OpenClaw session（默认 "main"），只从 config.py 读取
- `send_message()`: 发送消息触发 AI 处理

**连接参数限制**:
- `client.id`: 必须是 OpenClaw 预定义常量（如 "gateway-client"）
- `client.mode`: 必须是预定义常量（如 "backend"）

### 4b. OpenClawConversationController (openclaw_conversation.py)
OpenClaw 连续对话控制器，唤醒词触发后进入独立的 VAD → ASR → OpenClaw → TTS 循环：
- 使用 SherpaASR（SenseVoice 模型）做本地离线语音识别
- VAD 两步检测：先检测语音开始（on_speech），再等待语音结束（on_silence），中间 hook VAD 帧录制完整语音
- TTS 播放时通过 `stop_recording` / `start_recording` RPC 控制远端 arecord 进程（物理静音防回声）
- 播放后用 `_wait_for_silence` (VAD) 确认环境安静再恢复监听
- 支持退出关键词、超时退出、提示音

**回声防护的两层机制**:
1. `stop_recording` → kill 远端 arecord → 麦克风物理静音（音箱上有效）
2. `_wait_for_silence` → VAD 检测环境安静（本地调试也有效的兜底方案）

### 4c. SherpaASR (services/audio/asr/sherpa.py)
离线语音识别，基于 sherpa-onnx SenseVoice 模型：
- 模型目录自动扫描 `core/models/` 下的 `sherpa-onnx-sense-voice-*` 子目录
- 惰性加载：首次调用 `asr()` 时才加载模型
- 输入：PCM int16 bytes；输出：识别文本

### 5. SpeakerManager (services/speaker.py)
音箱控制接口：
- `play(text/url/buffer)`: 播放文字/链接/音频流
- `wake_up()`: 唤醒/休眠小爱
- `abort_xiaoai()`: 中断小爱当前操作
- `ask_xiaoai()`: 让小爱执行指令

### 6. 唤醒会话系统 (wakeup_session.py)
- `WakeupSessionManager.wakeup()`: 触发小智唤醒流程
- `before_wakeup`: 唤醒前回调（在 config.py 中配置）
- `after_wakeup`: 唤醒后回调

**重要**:
- 它不是通用事件总线，而是“小智唤醒会话状态机”。
- 只允许把 `on_speech` / `on_silence` 这类外部探测信号作为待等待事件缓存；不要把 `on_wakeup` / `on_interrupt` 这类控制步骤缓存进等待队列，否则会出现 `on_wakeup != on_speech` 这类误判。

### 7. XiaoAIConversationController (xiaoai_conversation.py)
- 管理小爱自己的连续对话状态
- 处理“小爱监听超时后是否继续唤醒”
- 处理小爱播放器事件对连续对话的影响

**边界约束**:
- 小爱的连续对话和小智的唤醒/会话超时是两套机制，不能混为一谈。
- 小智超时退出时，不应打印“小爱停止连续对话”这类日志。
- 小爱侧收到 `AudioPlayer` 事件时，只能在“小爱连续对话确实处于激活状态”时才允许停止连续对话。

### 8. Rust 原生扩展 (native/)
通过 [maturin](https://www.maturin.rs/) 编译的 Rust Python 扩展，提供高性能底层服务：
- `lib.rs`: 扩展入口，使用 PyO3 绑定；暴露 `stop_recording()` / `start_recording()` RPC 用于远程控制音箱麦克风
- `server.rs`: WebSocket 音频服务器（端口 4399）
- `python.rs`: Python API 暴露（`open_xiaoai_server` 模块）
- `tts/`: TTS 音频处理模块（HTTP 流式请求、MP3 解码、PCM 直通、流式缓冲、延迟测试接口）

**编译产物**: `open_xiaoai_server` Python 模块，供 `main.py` 调用

## 配置说明 (config.py)

```python
APP_CONFIG = {
    "wakeup": {
        "keywords": ["你好小智", "贾维斯"],  # 自定义唤醒词
        "timeout": 20,                          # 唤醒状态超时（秒）
        "before_wakeup": before_wakeup,         # 唤醒前回调
        "after_wakeup": after_wakeup,           # 退出唤醒回调
    },
    "vad": {
        "threshold": 0.10,      # 语音检测阈值
    },
    "xiaozhi": {
        "OTA_URL": "...",       # 小智 OTA 地址
        "WEBSOCKET_URL": "...", # 小智 WebSocket 地址
    },
    "openclaw": {
        "url": "ws://localhost:18789",
        "token": "",                  # OpenClaw 认证令牌
        "session_key": "main",        # OpenClaw session 标识（仅从 config.py 读取）
        "tts_enabled": False,         # 启用 Doubao TTS 播放 OpenClaw 回复
        "ack_timeout": 30,            # 等待 OpenClaw accepted 回执的超时时间（秒）
        "response_timeout": 120,      # 等待 Agent 完整回复的超时时间（秒）
        # "tts_speaker": "...",       # 可选：自定义音色，不设置则使用 tts.doubao.default_speaker
        # "tts_speed": 1.0,           # 可选：语速（0.5-2.0）
    },
    "tts": {
        "doubao": {
            "app_id": "...",
            "access_key": "...",
            "stream": True,                # 推荐默认值：边合成边播放
            "audio_format": "pcm",         # 推荐默认值：局域网稳定环境下首音更快、播放更顺
            # "audio_format": "auto",      # 可选：短文本用 pcm，长文本用 mp3
            # "auto_pcm_max_chars": 120,   # 可选：audio_format=auto 时的 PCM 阈值
        }
    }
}
```

## 开发规范

### 代码风格
- 使用中文注释和文档字符串
- 使用英文生成 commit message
- 类型提示: `dict[str, asyncio.Future]`

### 异步编程
- 所有 I/O 操作使用 `async/await`
- 线程安全使用 `asyncio.run_coroutine_threadsafe()`
- 全局事件循环在 `MainApp._run_event_loop()` 中运行

**补充约束**:
- `MainApp.loop` 是业务协程的主调度循环。
- `XiaoAI.async_loop` 仅用于承接原生扩展回调和小爱事件桥接，不要把新的业务状态机优先挂到这条 loop 上。

### 日志规范
- 所有运行时日志都必须带模块前缀，例如 `[Main]`、`[XiaoAI]`、`[Wakeup]`、`[KWS]`、`[VAD]`、`[OpenClaw]`。
- 正常运行路径禁止使用裸 `print` 输出日志；应使用 `core.utils.logger.logger`。
- 唯一允许的裸输出是启动时的 ASCII banner，它是展示性输出，不视为普通运行日志。
- 调试辅助输出应优先使用 `DEBUG` 级别，不要污染 `INFO` 级别。
- `logger.wakeup()`、`logger.user_speech()`、`logger.ai_response()`、`logger.vad_event()`、`logger.kws_event()` 这类 helper 也必须显式携带模块语义，不能产出无模块前缀日志。

### 历史兼容说明
- `CLI` 环境变量不再作为唤醒、VAD、KWS 的功能开关使用。
- 后续不要再引入依赖 `CLI` 的运行时分支；如需区分运行模式，应使用明确的功能开关或配置项。

### 添加新功能
1. 在 `config.py` 中添加配置项
2. 在 `core/` 下创建模块
3. 在 `MainApp` 中初始化管理
4. 通过 `ref.py` 注册全局访问点

### 调试技巧
- 日志使用 `core.utils.logger.logger`
- 设置环境变量 `DEBUG=1` 开启详细日志
- API Server 提供 `/api/health` 健康检查端点

## 常见集成模式

### 模式 1: 仅小爱模式（默认）
```python
# 环境变量: XIAOZHI_ENABLE=0
MainApp.instance(enable_xiaozhi=False)
# 只启动小爱服务，无 AI 对话功能
```

**兼容约定**:
- 当 `XIAOZHI_ENABLE=0` 时，必须允许跳过 KWS 相关初始化。
- `core/services/audio/kws/keywords.py` 在仅小爱模式下应直接退出成功，不能因为缺少 `tokens.txt`、`bpe.model` 或 `sherpa_onnx` 依赖导致主程序启动失败。
- 这里的“跳过”仅指跳过关键词预生成步骤，并继续启动主服务。
- `scripts/start.sh` / `scripts/start.bat` 在仅小爱模式下也不应检查、下载或读取 `core/models/` 下的 KWS/VAD 模型文件。
- 当 `XIAOZHI_ENABLE=1` 时，关键词预生成失败应视为启动失败，不能继续进入主服务。

### 模式 2: 小智 AI 模式
```python
# 环境变量: XIAOZHI_ENABLE=1
MainApp.instance(enable_xiaozhi=True)
# 启动 VAD + KWS，唤醒后连接小智 AI
```

### 模式 3: OpenClaw 代理模式
```python
# 环境变量: OPENCLAW_ENABLED=1
# 用户说"让龙虾 xxx" -> 转发到 OpenClaw -> 小爱播放结果
# 如果 tts_enabled=True，OpenClaw 的回复会自动通过 Doubao TTS 播放
await app.send_to_openclaw("用户指令")
```

### 模式 4: OpenClaw 连续对话模式
```python
# XIAOZHI_ENABLE=1 + OPENCLAW_ENABLED=1
# config.py 的 before_wakeup 中:
#   if "龙虾" in text: return "openclaw"
# 唤醒词触发后进入本地 VAD → ASR → OpenClaw → TTS 循环
# 说"退出"/"停止"/"再见"退出对话
```

### 模式 5: 混合模式
```python
# XIAOZHI_ENABLE=1 + OPENCLAW_ENABLED=1
# - "你好小智" -> 唤醒小智 AI
# - "让龙虾 xxx" -> OpenClaw 代理
# - 其他指令 -> 小爱原生处理
```

## 重要依赖

- `open_xiaoai_server`: Rust 实现的底层服务（音频采集/播放）
- `sherpa-onnx`: 关键词唤醒
- `silero-vad`: 语音活动检测
- `websockets`: WebSocket 客户端
- `aiohttp`: HTTP API Server

## 测试命令

所有命令都在项目根目录下执行：

```bash
# 运行仅小爱模式
uv run main.py

# 运行小智 AI 模式
XIAOZHI_ENABLE=1 uv run main.py

# 运行带 API Server
API_SERVER_ENABLE=1 XIAOZHI_ENABLE=1 uv run main.py

# 启用 OpenClaw
OPENCLAW_ENABLED=1 uv run main.py

# 无音箱流式冒烟测试
python3 tests/test_tts_stream.py

# 比较长文本 mp3 / pcm 流式时延
python3 tests/test_tts_latency.py --formats mp3,pcm --rounds 3 --repeat 8
```

## 参考资源

- 项目主页: https://github.com/coderzc/open-xiaoai-bridge
- 刷机教程: https://github.com/idootop/open-xiaoai/blob/main/docs/flash.md
- Client 端补丁: https://github.com/idootop/open-xiaoai/blob/main/packages/client-rust/README.md
