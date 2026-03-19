use open_xiaoai::services::audio::config::AudioConfig;
use open_xiaoai::services::connect::message::MessageManager;
use open_xiaoai::services::connect::rpc::RPC;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde_json::json;
use server::AppServer;

pub mod macros;
pub mod python;
pub mod server;
pub mod tts;

#[pyfunction]
fn on_output_data(py: Python, data: Py<PyBytes>) -> PyResult<Bound<PyAny>> {
    let bytes = data.as_bytes(py).to_vec();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let _ = MessageManager::instance()
            .send_stream("play", bytes, None)
            .await;
        Ok(())
    })
}

#[pyfunction]
fn start_server(py: Python) -> PyResult<Bound<PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async {
        AppServer::run().await;
        Ok(())
    })
}

#[pyfunction]
fn run_shell(py: Python, script: String, timeout_millis: f64) -> PyResult<Bound<PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let res = RPC::instance()
            .call_remote(
                "run_shell",
                Some(json!(script)),
                Some(timeout_millis as u64),
            )
            .await;
        let result = match res {
            Err(e) => format!("run_shell error: {}", e),
            Ok(res) => serde_json::to_string(&res.data.unwrap()).unwrap(),
        };
        Ok(result)
    })
}

/// Stop the remote arecord process (mutes the microphone).
#[pyfunction]
fn stop_recording(py: Python) -> PyResult<Bound<PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async {
        let _ = RPC::instance()
            .call_remote("stop_recording", None, None)
            .await;
        Ok(())
    })
}

/// Restart the remote arecord process (unmutes the microphone).
#[pyfunction]
fn start_recording(py: Python) -> PyResult<Bound<PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async {
        let _ = RPC::instance()
            .call_remote(
                "start_recording",
                Some(json!(AudioConfig {
                    pcm: "noop".into(),
                    channels: 1,
                    bits_per_sample: 16,
                    sample_rate: 16000,
                    period_size: 1440 / 4,
                    buffer_size: 1440,
                })),
                None,
            )
            .await;
        Ok(())
    })
}

#[pymodule]
fn open_xiaoai_server(_py: Python, m: Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(start_server, &m)?)?;
    m.add_function(wrap_pyfunction!(on_output_data, &m)?)?;
    m.add_function(wrap_pyfunction!(run_shell, &m)?)?;
    m.add_function(wrap_pyfunction!(stop_recording, &m)?)?;
    m.add_function(wrap_pyfunction!(start_recording, &m)?)?;
    crate::python::init_module(&m)?;
    crate::tts::init_module(&m)?;
    Ok(())
}
