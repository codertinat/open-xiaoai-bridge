pub mod decoder;
pub mod doubao;

use crate::tts::decoder::{decode_audio_to_pcm, StreamingDecoder};
use crate::tts::doubao::DoubaoStreamClient;
use open_xiaoai::services::connect::message::MessageManager;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde_json::json;
use std::time::Instant;
use tokio::sync::mpsc;

const STREAM_BUFFER_THRESHOLD: usize = 8192;
const PLAY_CHUNK_SIZE: usize = 1024 * 1024; // 1MB chunks for WebSocket

/// Send PCM data to device, auto-chunking if larger than PLAY_CHUNK_SIZE.
async fn send_pcm(pcm: Vec<u8>) {
    if pcm.len() <= PLAY_CHUNK_SIZE {
        let _ = MessageManager::instance()
            .send_stream("play", pcm, None)
            .await;
    } else {
        let mut offset = 0;
        while offset < pcm.len() {
            let end = (offset + PLAY_CHUNK_SIZE).min(pcm.len());
            let _ = MessageManager::instance()
                .send_stream("play", pcm[offset..end].to_vec(), None)
                .await;
            offset = end;
        }
    }
}

/// Stream TTS: fetch audio from Doubao API, decode to PCM in chunks, and play via WebSocket.
/// Supports MP3, OGG Vorbis, WAV, FLAC formats.
#[pyfunction]
#[pyo3(signature = (text, app_id, access_key, resource_id, speaker, speed=1.0, format="mp3".to_string(), sample_rate=24000, emotion=None, context_texts=None))]
pub fn tts_stream_play(
    py: Python<'_>,
    text: String,
    app_id: String,
    access_key: String,
    resource_id: String,
    speaker: String,
    speed: f32,
    format: String,
    sample_rate: u32,
    emotion: Option<String>,
    context_texts: Option<Vec<String>>,
) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let started_at = Instant::now();
        let client = DoubaoStreamClient::new(app_id, access_key, resource_id, speaker);

        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(16);

        let fetch_handle = tokio::spawn({
            let text = text.clone();
            let format = format.clone();
            async move {
                client
                    .stream_audio(&text, &format, sample_rate, speed, context_texts, emotion, tx)
                    .await
            }
        });

        let mut decoder = StreamingDecoder::new(&format, sample_rate);
        let mut accumulated_size: usize = 0;
        let mut first_audio_chunk_logged = false;
        let mut first_playable_pcm_logged = false;

        while let Some(chunk) = rx.recv().await {
            if !first_audio_chunk_logged {
                crate::pylog!(
                    "[TTS] Stream first encoded chunk arrived after {} ms ({} bytes)",
                    started_at.elapsed().as_millis(),
                    chunk.len()
                );
                first_audio_chunk_logged = true;
            }

            accumulated_size += chunk.len();
            decoder.feed(&chunk);

            if accumulated_size >= STREAM_BUFFER_THRESHOLD {
                match decoder.decode_all() {
                    Ok(pcm) if !pcm.is_empty() => {
                        if !first_playable_pcm_logged {
                            crate::pylog!(
                                "[TTS] Stream first playable PCM ready after {} ms ({} bytes)",
                                started_at.elapsed().as_millis(),
                                pcm.len()
                            );
                            first_playable_pcm_logged = true;
                        }
                        send_pcm(pcm).await;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        crate::pylog!("[TTS] Decode error (continuing): {}", e);
                    }
                }
                accumulated_size = 0;
            }
        }

        match decoder.decode_all() {
            Ok(pcm) if !pcm.is_empty() => {
                if !first_playable_pcm_logged {
                    crate::pylog!(
                        "[TTS] Stream single playable PCM ready after {} ms ({} bytes)",
                        started_at.elapsed().as_millis(),
                        pcm.len()
                    );
                }
                send_pcm(pcm).await;
            }
            Ok(_) => {}
            Err(e) => {
                crate::pylog!("[TTS] Final decode error: {}", e);
            }
        }

        if let Ok(Err(e)) = fetch_handle.await {
            crate::pylog!("[TTS] Stream fetch error: {}", e);
        }

        crate::pylog!(
            "[TTS] Stream playback completed in {} ms",
            started_at.elapsed().as_millis()
        );
        Ok(())
    })
}

/// Non-streaming TTS: fetch all audio, decode to PCM, then play.
#[pyfunction]
#[pyo3(signature = (text, app_id, access_key, resource_id, speaker, speed=1.0, format="mp3".to_string(), sample_rate=24000, emotion=None, context_texts=None))]
pub fn tts_play(
    py: Python<'_>,
    text: String,
    app_id: String,
    access_key: String,
    resource_id: String,
    speaker: String,
    speed: f32,
    format: String,
    sample_rate: u32,
    emotion: Option<String>,
    context_texts: Option<Vec<String>>,
) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let started_at = Instant::now();
        let client = DoubaoStreamClient::new(app_id, access_key, resource_id, speaker);

        let encoded_audio = client
            .fetch_audio(&text, &format, sample_rate, speed, context_texts, emotion)
            .await
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;

        crate::pylog!(
            "[TTS] Non-stream fetch completed in {} ms, synthesized {} bytes ({})",
            started_at.elapsed().as_millis(),
            encoded_audio.len(),
            format
        );

        let pcm = decode_audio_to_pcm(&encoded_audio, &format, sample_rate)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;

        crate::pylog!(
            "[TTS] Non-stream PCM ready after {} ms ({} bytes)",
            started_at.elapsed().as_millis(),
            pcm.len()
        );

        send_pcm(pcm).await;

        crate::pylog!(
            "[TTS] Non-stream playback completed in {} ms",
            started_at.elapsed().as_millis()
        );
        Ok(())
    })
}

/// Stream TTS without playback and return timing/statistics as JSON.
#[pyfunction]
#[pyo3(signature = (text, app_id, access_key, resource_id, speaker, speed=1.0, format="mp3".to_string(), sample_rate=24000, emotion=None, context_texts=None))]
pub fn tts_stream_collect(
    py: Python<'_>,
    text: String,
    app_id: String,
    access_key: String,
    resource_id: String,
    speaker: String,
    speed: f32,
    format: String,
    sample_rate: u32,
    emotion: Option<String>,
    context_texts: Option<Vec<String>>,
) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let started_at = Instant::now();
        let client = DoubaoStreamClient::new(app_id, access_key, resource_id, speaker);
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(16);

        let fetch_handle = tokio::spawn({
            let text = text.clone();
            let format = format.clone();
            async move {
                client
                    .stream_audio(&text, &format, sample_rate, speed, context_texts, emotion, tx)
                    .await
            }
        });

        let mut decoder = StreamingDecoder::new(&format, sample_rate);
        let mut accumulated_size: usize = 0;
        let mut encoded_chunks: usize = 0;
        let mut encoded_bytes: usize = 0;
        let mut pcm_chunks: usize = 0;
        let mut pcm_bytes: usize = 0;
        let mut first_encoded_ms: Option<u128> = None;
        let mut first_pcm_ms: Option<u128> = None;

        while let Some(chunk) = rx.recv().await {
            if first_encoded_ms.is_none() {
                first_encoded_ms = Some(started_at.elapsed().as_millis());
            }

            encoded_chunks += 1;
            encoded_bytes += chunk.len();
            accumulated_size += chunk.len();
            decoder.feed(&chunk);

            if accumulated_size >= STREAM_BUFFER_THRESHOLD {
                match decoder.decode_all() {
                    Ok(pcm) if !pcm.is_empty() => {
                        if first_pcm_ms.is_none() {
                            first_pcm_ms = Some(started_at.elapsed().as_millis());
                        }
                        pcm_chunks += 1;
                        pcm_bytes += pcm.len();
                    }
                    Ok(_) => {}
                    Err(e) => {
                        return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "stream decode failed: {}",
                            e
                        )));
                    }
                }
                accumulated_size = 0;
            }
        }

        match decoder.decode_all() {
            Ok(pcm) if !pcm.is_empty() => {
                if first_pcm_ms.is_none() {
                    first_pcm_ms = Some(started_at.elapsed().as_millis());
                }
                pcm_chunks += 1;
                pcm_bytes += pcm.len();
            }
            Ok(_) => {}
            Err(e) => {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "final stream decode failed: {}",
                    e
                )));
            }
        }

        if let Ok(Err(e)) = fetch_handle.await {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "stream fetch failed: {}",
                e
            )));
        }

        Ok(json!({
            "ok": true,
            "format": format,
            "sample_rate": sample_rate,
            "encoded_chunks": encoded_chunks,
            "encoded_bytes": encoded_bytes,
            "pcm_chunks": pcm_chunks,
            "pcm_bytes": pcm_bytes,
            "first_encoded_ms": first_encoded_ms,
            "first_pcm_ms": first_pcm_ms,
            "total_ms": started_at.elapsed().as_millis(),
        })
        .to_string())
    })
}

/// Decode encoded audio bytes (MP3/OGG/WAV/FLAC) to PCM (16-bit mono).
/// Returns PCM bytes. This is a sync function for file upload use cases.
#[pyfunction]
#[pyo3(signature = (audio_data, format="mp3", sample_rate=24000))]
pub fn decode_audio<'py>(
    py: Python<'py>,
    audio_data: &[u8],
    format: &str,
    sample_rate: u32,
) -> PyResult<Bound<'py, PyBytes>> {
    let pcm = decode_audio_to_pcm(audio_data, format, sample_rate)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;
    Ok(PyBytes::new(py, &pcm))
}

pub fn init_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(tts_stream_play, m)?)?;
    m.add_function(wrap_pyfunction!(tts_play, m)?)?;
    m.add_function(wrap_pyfunction!(tts_stream_collect, m)?)?;
    m.add_function(wrap_pyfunction!(decode_audio, m)?)?;
    Ok(())
}
