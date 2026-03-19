import asyncio

from core.ref import (
    get_app,
    get_audio_codec,
    get_kws,
    get_speaker,
    get_vad,
    get_xiaoai,
    get_xiaozhi,
    set_speech_frames,
)
from core.services.protocols.typing import AbortReason, DeviceState, ListeningMode
from core.utils.config import ConfigManager
from core.utils.logger import logger


class WakeupStep:
    idle = "idle"
    on_interrupt = "on_interrupt"
    on_wakeup = "on_wakeup"
    on_tts_start = "on_tts_start"
    on_tts_end = "on_tts_end"
    on_speech = "on_speech"
    on_silence = "on_silence"


class WakeupSessionManager:
    def __init__(self):
        self.session_id = 0
        self.current_step = WakeupStep.idle
        self.next_step_future = None
        self.pending_step = None
        self.pending_step_data = None
        self.pending_step_session_id = None
        self.config = ConfigManager.instance()

    def _get_loop(self):
        app = get_app()
        if app:
            return app.loop
        return get_xiaoai().async_loop

    def update_step(self, step: WakeupStep, step_data=None):
        self.current_step = step
        if self.next_step_future and not self.next_step_future.done():
            loop = self._get_loop()
            loop.call_soon_threadsafe(
                self.next_step_future.set_result, (step, step_data)
            )
            self.next_step_future = None
            return

        if step in {WakeupStep.on_speech, WakeupStep.on_silence}:
            # Buffer detector signals so they won't be lost if the waiter
            # becomes ready a few milliseconds later.
            self.pending_step = step
            self.pending_step_data = step_data
            self.pending_step_session_id = self.session_id

    def _clear_pending_step(self):
        self.pending_step = None
        self.pending_step_data = None
        self.pending_step_session_id = None

    async def wait_next_step(self, timeout=None):
        current_session = self.session_id
        loop = self._get_loop()

        if (
            self.pending_step is not None
            and self.pending_step_session_id == current_session
        ):
            step, step_data = self.pending_step, self.pending_step_data
            self._clear_pending_step()
            return (step, step_data)

        self.next_step_future = loop.create_future()

        async def _timeout(wait_seconds):
            elapsed = 0
            while elapsed < wait_seconds:
                elapsed += 1
                await asyncio.sleep(1)
            return ("timeout", None)

        futures = [self.next_step_future]

        if timeout:
            futures.append(loop.create_task(_timeout(timeout)))

        done, _ = await asyncio.wait(
            futures,
            return_when=asyncio.FIRST_COMPLETED,
        )
        if current_session != self.session_id:
            return ("interrupted", None)
        return list(done)[0].result()

    def on_interrupt(self):
        self.session_id += 1
        self._clear_pending_step()
        logger.info("[Wakeup] Interrupt received from XiaoAI")
        self.update_step(WakeupStep.on_interrupt)
        self.start_session()

    def on_wakeup(self):
        self.session_id += 1
        self._clear_pending_step()
        logger.info("[Wakeup] Wakeup session started")
        self.update_step(WakeupStep.on_wakeup)
        self.start_session()

    def on_tts_end(self, session_id):
        if self.current_step not in [WakeupStep.on_tts_start]:
            return
        self.session_id += 1
        self._clear_pending_step()
        self.update_step(WakeupStep.on_tts_end)
        self.start_session()

    def on_tts_start(self, session_id):
        self.update_step(WakeupStep.on_tts_start)

    def on_speech(self, speech_buffer: bytes):
        self.update_step(WakeupStep.on_speech, speech_buffer)

    def on_silence(self):
        self.update_step(WakeupStep.on_silence)

    def start_session(self):
        future = asyncio.run_coroutine_threadsafe(
            self.__start_session(), self._get_loop()
        )

        def _log_result(done_future):
            try:
                done_future.result()
            except Exception as exc:
                logger.error(
                    f"Session task failed: {type(exc).__name__}: {exc}",
                    module="Wakeup",
                )

        future.add_done_callback(_log_result)

    async def __start_session(self):
        vad = get_vad()
        codec = get_audio_codec()
        speaker = get_speaker()
        xiaozhi = get_xiaozhi()

        if not xiaozhi or not xiaozhi.protocol:
            logger.warning("[Wakeup] XiaoZhi is not ready, skip wakeup session")
            return

        xiaozhi.set_device_state(DeviceState.IDLE)
        await xiaozhi.send_abort_speaking(AbortReason.ABORT)

        if self.current_step == WakeupStep.on_interrupt:
            return

        if self.current_step in [WakeupStep.on_tts_end]:
            vad.resume("silence")
            step, _ = await self.wait_next_step()
            if step != WakeupStep.on_silence:
                logger.warning(
                    f"{step} != {WakeupStep.on_silence} -- tts",
                    module="Wakeup",
                )
                return

        vad.resume("speech")
        step, speech_buffer = await self.wait_next_step(
            timeout=self.config.get_app_config("wakeup.timeout", 20)
        )
        if step == "timeout":
            xiaozhi.set_device_state(DeviceState.IDLE)
            logger.info("👋 已退出唤醒", module="Wakeup")
            after_wakeup = self.config.get_app_config("wakeup.after_wakeup")
            if after_wakeup:
                await after_wakeup(speaker)
            return
        if step != WakeupStep.on_speech:
            logger.warning(
                f"{step} != {WakeupStep.on_speech} -- timeout",
                module="Wakeup",
            )
            return

        logger.debug(
            f"开始说话，speech_buffer size: {len(speech_buffer)}",
            module="Wakeup",
        )
        set_speech_frames(speech_buffer)
        codec.input_stream.start_stream()
        await xiaozhi.send_start_listening(ListeningMode.MANUAL)
        xiaozhi.set_device_state(DeviceState.LISTENING)

        vad.resume("silence")
        step, _ = await self.wait_next_step()
        if step != WakeupStep.on_silence:
            logger.warning(
                f"{step} != {WakeupStep.on_silence} -- silence",
                module="Wakeup",
            )
            return

        logger.info("说话结束", module="Wakeup")
        await xiaozhi.send_stop_listening()
        xiaozhi.set_device_state(DeviceState.IDLE)

    async def wakeup(self, text, source):
        before_wakeup = self.config.get_app_config("wakeup.before_wakeup")
        kws = get_kws()
        logger.info(f"[Wakeup] Received wakeup request from {source}: {text}")
        if kws:
            kws.pause()
        should_wakeup = await before_wakeup(
            get_speaker(),
            text,
            source,
            get_xiaozhi(),
            get_xiaoai(),
            get_app(),
        )
        if kws:
            kws.resume()
        logger.info(f"[Wakeup] before_wakeup returned: {should_wakeup}")
        if should_wakeup == "openclaw":
            await self._start_openclaw_conversation()
        elif should_wakeup:
            self.on_wakeup()

    async def _start_openclaw_conversation(self):
        """Start an OpenClaw continuous conversation session.

        This runs independently of the XiaoZhi session state machine.
        KWS is paused during the conversation and resumed when done.
        """
        from core.openclaw_conversation import OpenClawConversationController

        kws = get_kws()
        if kws:
            kws.pause()
        try:
            controller = OpenClawConversationController()
            await controller.start()
        except Exception as exc:
            logger.error(
                f"[Wakeup] OpenClaw conversation failed: {type(exc).__name__}: {exc}",
                module="Wakeup",
            )
        finally:
            if kws:
                kws.resume()


EventManager = WakeupSessionManager()
Step = WakeupStep
