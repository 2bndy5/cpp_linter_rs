use pyo3::prelude::*;
use tokio::runtime::Builder;

use ::cpp_linter::run::run_main;

/// A wrapper for the ``::cpp_linter::run::run_main()```
#[pyfunction]
fn main(args: Vec<String>) -> PyResult<i32> {
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { Ok(run_main(args).await) })
}

/// The python binding for the cpp_linter package. It only exposes a ``main()`` function
/// that is used as the entrypoint script.
#[pymodule]
fn cpp_linter(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(main, m)?)?;
    Ok(())
}
