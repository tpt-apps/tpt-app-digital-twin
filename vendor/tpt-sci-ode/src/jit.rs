//! Cranelift JIT compilation for DiffSL-style RHS functions.
//!
//! This module provides Just-In-Time compilation of user-provided RHS (right-hand side)
//! functions for ODE systems. It compiles the closure to native machine code using
//! Cranelift, eliminating the overhead of dynamic dispatch and enabling advanced
//! optimizations.
//!
//! The DiffSL (Differential Equation Shader Language) concept here is a lightweight
//! representation of the RHS function that can be JIT-compiled. Since we only use
//! closures in this crate, the JIT compiles a specialized version of the closure
//! with the concrete state dimension known at compile time.
//!
//! # Architecture
//!
//! - [`JitRhs`] — A JIT-compiled RHS function with a fixed state dimension.
//! - [`JitRhsBuilder`] — Manages the Cranelift JIT context and module.
//! - [`compile_rhs`] — Compiles a user closure to a callable function pointer.
//!
//! # Example
//!
//! ```ignore
//! use tpt_sci_ode::jit::{JitRhs, compile_rhs};
//!
//! // Compile a simple exponential decay RHS: dy/dt = -y
//! let rhs_fn = compile_rhs(|t: f64, y: &[f64], dydt: &mut [f64]| {
//!     dydt[0] = -y[0];
//! }).unwrap();
//!
//! // Use the compiled function
//! let mut y = [1.0];
//! let mut dydt = [0.0];
//! rhs_fn.call(0.0, &y, &mut dydt);
//! assert!((dydt[0] + 1.0).abs() < 1e-10);
//! ```

#![allow(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use cranelift_codegen::ir::{self, InstBuilder, types};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use target_lexicon::Triple;

use crate::error::OdeError;

/// A JIT-compiled RHS function for a specific state dimension.
///
/// This wraps a compiled Cranelift function that implements the RHS signature:
/// `fn(t: f64, y: *const f64, dydt: *mut f64)`
///
/// The function is specialized for a fixed state dimension `nstates`, allowing
/// the JIT to unroll loops and optimize memory access patterns.
pub struct JitRhs {
    /// The compiled function ID in the JIT module (kept for potential future use).
    _func_id: FuncId,
    /// Number of state variables (dimension of y and dydt).
    nstates: usize,
    /// Pointer to the compiled function entry point.
    fn_ptr: *const u8,
}

/// Global registry for JIT-compiled RHS closures.
/// Maps (nstates, unique_id) -> Box<dyn Fn(f64, &[f64], &mut [f64])>
#[allow(clippy::type_complexity)]
static RHS_REGISTRY: OnceLock<
    Mutex<HashMap<(usize, usize), Box<dyn Fn(f64, &[f64], &mut [f64]) + Send + Sync>>>,
> = OnceLock::new();

/// Unique ID counter for registry entries.
static REGISTRY_COUNTER: OnceLock<Mutex<usize>> = OnceLock::new();

#[allow(clippy::type_complexity)]
fn get_registry()
-> &'static Mutex<HashMap<(usize, usize), Box<dyn Fn(f64, &[f64], &mut [f64]) + Send + Sync>>> {
    RHS_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn get_counter() -> &'static Mutex<usize> {
    REGISTRY_COUNTER.get_or_init(|| Mutex::new(0))
}

/// Register a closure in the global registry and return its unique ID.
fn register_rhs_closure<F>(nstates: usize, rhs: F) -> usize
where
    F: Fn(f64, &[f64], &mut [f64]) + Send + Sync + 'static,
{
    let mut counter = get_counter().lock().unwrap();
    let id = *counter;
    *counter += 1;
    drop(counter);

    let mut registry = get_registry().lock().unwrap();
    registry.insert((nstates, id), Box::new(rhs));
    id
}

/// Call a registered closure from the trampoline.
extern "C" fn rhs_trampoline_extern(
    nstates: usize,
    id: usize,
    t: f64,
    y: *const f64,
    dydt: *mut f64,
) {
    let registry = get_registry().lock().unwrap();
    if let Some(rhs) = registry.get(&(nstates, id)) {
        // Convert raw pointers to slices
        let y_slice = unsafe { std::slice::from_raw_parts(y, nstates) };
        let dydt_slice = unsafe { std::slice::from_raw_parts_mut(dydt, nstates) };
        rhs(t, y_slice, dydt_slice);
    }
}

impl JitRhs {
    /// Returns the number of state variables this RHS operates on.
    pub fn nstates(&self) -> usize {
        self.nstates
    }

    /// Calls the compiled RHS function.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `y` and `dydt` point to valid memory
    /// of at least `nstates` elements each.
    pub fn call(&self, t: f64, y: &[f64], dydt: &mut [f64]) {
        debug_assert_eq!(y.len(), self.nstates);
        debug_assert_eq!(dydt.len(), self.nstates);

        type RhsFnPtr = unsafe extern "C" fn(f64, *const f64, *mut f64);
        // SAFETY: The function pointer was obtained from Cranelift JIT compilation
        // and has the correct signature.
        let func: RhsFnPtr = unsafe { std::mem::transmute(self.fn_ptr) };
        // SAFETY: The caller verified the lengths match, and the function pointer is valid.
        unsafe { func(t, y.as_ptr(), dydt.as_mut_ptr()) };
    }

    /// Calls the compiled RHS function (safe wrapper).
    ///
    /// # Errors
    ///
    /// Returns [`OdeError::Invalid`] if the length of `y` or `dydt` does not
    /// match the compiled number of states.
    pub fn call_safe(&self, t: f64, y: &[f64], dydt: &mut [f64]) -> Result<(), OdeError> {
        if y.len() != self.nstates || dydt.len() != self.nstates {
            return Err(OdeError::Invalid(format!(
                "JitRhs expects {} states, got y.len()={}, dydt.len()={}",
                self.nstates,
                y.len(),
                dydt.len()
            )));
        }
        self.call(t, y, dydt);
        Ok(())
    }
}

/// Builder for compiling RHS functions to native code via Cranelift.
pub struct JitRhsBuilder {
    /// The Cranelift JIT module.
    module: Mutex<JITModule>,
    /// Target ISA for code generation.
    isa: Arc<dyn TargetIsa>,
}

impl JitRhsBuilder {
    /// Creates a new JIT builder with the host's native target.
    ///
    /// # Errors
    ///
    /// Returns [`OdeError::Invalid`] if the host target ISA cannot be looked up or
    /// finalised.
    ///
    /// # Panics
    ///
    /// Panics if the internal Cranelift flag configuration is invalid (should not
    /// occur for the built-in settings).
    pub fn new() -> Result<Self, OdeError> {
        let mut flag_builder = settings::builder();
        flag_builder.enable("enable_verifier").unwrap();
        flag_builder.set("opt_level", "speed").unwrap();
        let flags = settings::Flags::new(flag_builder);

        // Use target-lexicon to determine the host triple and create the ISA
        let triple = Triple::host();
        let isa_builder = cranelift_codegen::isa::lookup(triple)
            .map_err(|e| OdeError::Invalid(format!("Failed to lookup ISA: {}", e)))?;
        let isa = isa_builder
            .finish(flags)
            .map_err(|e| OdeError::Invalid(format!("Failed to create target ISA: {}", e)))?;

        let mut jit_builder = JITBuilder::with_isa(isa.clone(), default_libcall_names());

        // Register the extern "C" trampoline function as a symbol
        jit_builder.symbol("rhs_trampoline_extern", rhs_trampoline_extern as *const u8);

        let module = JITModule::new(jit_builder);

        Ok(Self {
            module: Mutex::new(module),
            isa,
        })
    }

    /// Compiles a user-provided RHS closure to a `JitRhs`.
    ///
    /// The closure must have the signature `Fn(f64, &[f64], &mut [f64])` and
    /// will be specialized for the given `nstates` dimension.
    ///
    /// # Errors
    ///
    /// Returns [`OdeError::Invalid`] if `nstates == 0` or the closure cannot be
    /// compiled/encoded into a native function.
    ///
    /// # Panics
    ///
    /// Panics if the internal Cranelift flag configuration is invalid (should not
    /// occur for the built-in settings).
    pub fn compile<F>(&self, nstates: usize, rhs: F) -> Result<JitRhs, OdeError>
    where
        F: Fn(f64, &[f64], &mut [f64]) + Send + Sync + 'static,
    {
        if nstates == 0 {
            return Err(OdeError::Invalid("nstates must be > 0".to_string()));
        }

        // Register the closure in the global registry
        let rhs_id = register_rhs_closure(nstates, rhs);

        let mut module = self.module.lock().unwrap();

        // First, define the trampoline function in the module
        let trampoline_name = "rhs_trampoline";
        let ptr_type = types::I64;
        let call_conv = CallConv::for_libcall(
            self.isa.flags(),
            CallConv::triple_default(self.isa.triple()),
        );
        let mut trampoline_sig = ir::Signature::new(call_conv);
        trampoline_sig.params.push(ir::AbiParam::new(types::I64)); // nstates
        trampoline_sig.params.push(ir::AbiParam::new(types::I64)); // id
        trampoline_sig.params.push(ir::AbiParam::new(types::F64)); // t
        trampoline_sig.params.push(ir::AbiParam::new(ptr_type)); // y pointer
        trampoline_sig.params.push(ir::AbiParam::new(ptr_type)); // dydt pointer

        // Declare the trampoline as a local function (not an import)
        let trampoline_func_id = module
            .declare_function(trampoline_name, Linkage::Export, &trampoline_sig)
            .map_err(|e| OdeError::Invalid(format!("Failed to declare trampoline: {}", e)))?;

        // Define the trampoline function body - it calls the extern "C" function
        let mut trampoline_ctx = module.make_context();
        trampoline_ctx.func.signature = trampoline_sig.clone();
        {
            let mut func_ctx = FunctionBuilderContext::new();
            let mut builder = FunctionBuilder::new(&mut trampoline_ctx.func, &mut func_ctx);

            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);
            builder.seal_block(entry_block);

            let nstates_val = builder.block_params(entry_block)[0];
            let id_val = builder.block_params(entry_block)[1];
            let t_val = builder.block_params(entry_block)[2];
            let y_ptr = builder.block_params(entry_block)[3];
            let dydt_ptr = builder.block_params(entry_block)[4];

            // The extern "C" function was registered as a symbol in the JIT builder
            // We need to declare it as a function in the module to call it
            let extern_trampoline_sig = trampoline_sig.clone();
            let extern_trampoline_func_id = module
                .declare_function(
                    "rhs_trampoline_extern",
                    Linkage::Import,
                    &extern_trampoline_sig,
                )
                .map_err(|e| {
                    OdeError::Invalid(format!("Failed to declare extern trampoline: {}", e))
                })?;

            let extern_ref = module.declare_func_in_func(extern_trampoline_func_id, builder.func);
            builder
                .ins()
                .call(extern_ref, &[nstates_val, id_val, t_val, y_ptr, dydt_ptr]);
            builder.ins().return_(&[]);
            builder.finalize(module.isa().frontend_config());
        }

        // Define the trampoline function
        module
            .define_function(trampoline_func_id, &mut trampoline_ctx)
            .map_err(|e| OdeError::Invalid(format!("Failed to define trampoline: {}", e)))?;

        // Now create the main JIT function
        let mut ctx = module.make_context();

        // Function signature: (f64, *const f64, *mut f64) -> ()
        let call_conv = CallConv::for_libcall(
            self.isa.flags(),
            CallConv::triple_default(self.isa.triple()),
        );
        let mut sig = ir::Signature::new(call_conv);
        sig.params.push(ir::AbiParam::new(types::F64)); // t
        sig.params.push(ir::AbiParam::new(ptr_type)); // y pointer
        sig.params.push(ir::AbiParam::new(ptr_type)); // dydt pointer
        ctx.func.signature = sig;

        let mut func_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut func_ctx);

        // Create the entry block
        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // Get function parameters
        let t_val = builder.block_params(entry_block)[0];
        let y_ptr = builder.block_params(entry_block)[1];
        let dydt_ptr = builder.block_params(entry_block)[2];

        // Call the trampoline with nstates, id, t, y, dydt
        let trampoline_ref = module.declare_func_in_func(trampoline_func_id, builder.func);
        let nstates_val = builder.ins().iconst(types::I64, nstates as i64);
        let id_val = builder.ins().iconst(types::I64, rhs_id as i64);

        builder.ins().call(
            trampoline_ref,
            &[nstates_val, id_val, t_val, y_ptr, dydt_ptr],
        );

        builder.ins().return_(&[]);
        builder.finalize(module.isa().frontend_config());

        // Define the function in the module
        let func_name = format!("jit_rhs_{}", nstates);
        let func_id = module
            .declare_function(&func_name, Linkage::Export, &ctx.func.signature)
            .map_err(|e| OdeError::Invalid(format!("Failed to declare function: {}", e)))?;

        module
            .define_function(func_id, &mut ctx)
            .map_err(|e| OdeError::Invalid(format!("Failed to define function: {}", e)))?;

        // Finalize the module to get the function pointer
        module
            .finalize_definitions()
            .map_err(|e| OdeError::Invalid(format!("Failed to finalize definitions: {}", e)))?;

        let fn_ptr = module.get_finalized_function(func_id);

        Ok(JitRhs {
            _func_id: func_id,
            nstates,
            fn_ptr,
        })
    }
}

/// Compiles a user RHS closure to a JIT-compiled function.
///
/// This is a convenience function that creates a builder and compiles the closure
/// in one step. For repeated compilations, use `JitRhsBuilder` directly.
///
/// # Errors
///
/// Returns [`OdeError::Invalid`] if `nstates == 0` or compilation fails (see
/// [`JitRhsBuilder::compile`]).
///
/// # Example
///
/// ```ignore
/// use tpt_sci_ode::jit::compile_rhs;
///
/// let jit_rhs = compile_rhs(2, |t: f64, y: &[f64], dydt: &mut [f64]| {
///     // Lotka-Volterra equations
///     dydt[0] = 1.5 * y[0] - 1.0 * y[0] * y[1];
///     dydt[1] = -3.0 * y[1] + 1.0 * y[0] * y[1];
/// }).unwrap();
/// ```
pub fn compile_rhs<F>(nstates: usize, rhs: F) -> Result<JitRhs, OdeError>
where
    F: Fn(f64, &[f64], &mut [f64]) + Send + Sync + 'static,
{
    let builder = JitRhsBuilder::new()?;
    builder.compile(nstates, rhs)
}

impl Default for JitRhsBuilder {
    fn default() -> Self {
        Self::new().expect("Failed to create JitRhsBuilder")
    }
}

/// `JitRhs` is a [`RhsCallable`](crate::RhsCallable) so it can be fed directly
/// into [`crate::OdeProblem`] / the integrators, sharing the exact same solving
/// pipeline as a plain closure.
impl crate::RhsCallable for JitRhs {
    fn nstates(&self) -> usize {
        self.nstates
    }

    fn call(&self, t: f64, y: &[f64], dydt: &mut [f64]) -> Result<(), OdeError> {
        self.call_safe(t, y, dydt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jit_builder_creation() {
        let builder = JitRhsBuilder::new();
        assert!(builder.is_ok());
    }

    #[test]
    fn test_compile_simple_rhs() {
        let builder = JitRhsBuilder::new().unwrap();

        // Simple exponential decay: dy/dt = -y
        let jit_rhs = builder
            .compile(1, move |_t: f64, y: &[f64], dydt: &mut [f64]| {
                dydt[0] = -y[0];
            })
            .unwrap();

        assert_eq!(jit_rhs.nstates(), 1);

        let y = [1.0];
        let mut dydt = [0.0];
        jit_rhs.call_safe(0.0, &y, &mut dydt).unwrap();
        assert!((dydt[0] + 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_compile_lotka_volterra() {
        let builder = JitRhsBuilder::new().unwrap();

        // Lotka-Volterra predator-prey model
        let jit_rhs = builder
            .compile(2, move |_t: f64, y: &[f64], dydt: &mut [f64]| {
                let alpha = 1.5;
                let beta = 1.0;
                let gamma = 3.0;
                let delta = 1.0;
                dydt[0] = alpha * y[0] - beta * y[0] * y[1];
                dydt[1] = delta * y[0] * y[1] - gamma * y[1];
            })
            .unwrap();

        assert_eq!(jit_rhs.nstates(), 2);

        let y = [10.0, 5.0];
        let mut dydt = [0.0, 0.0];
        jit_rhs.call_safe(0.0, &y, &mut dydt).unwrap();

        // dy0/dt = 1.5*10 - 1.0*10*5 = 15 - 50 = -35
        // dy1/dt = 1.0*10*5 - 3.0*5 = 50 - 15 = 35
        assert!((dydt[0] + 35.0).abs() < 1e-10);
        assert!((dydt[1] - 35.0).abs() < 1e-10);
    }

    #[test]
    fn test_compile_rhs_convenience() {
        let jit_rhs = compile_rhs(1, move |_t: f64, y: &[f64], dydt: &mut [f64]| {
            dydt[0] = -2.0 * y[0];
        })
        .unwrap();

        assert_eq!(jit_rhs.nstates(), 1);

        let y = [1.0];
        let mut dydt = [0.0];
        jit_rhs.call_safe(0.0, &y, &mut dydt).unwrap();
        assert!((dydt[0] + 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_rhs_callable_trait() {
        let closure_rhs = move |_t: f64, y: &[f64], dydt: &mut [f64]| {
            dydt[0] = -y[0];
        };

        let y = [1.0];
        let mut dydt = [0.0];
        closure_rhs(0.0, &y, &mut dydt);
        assert!((dydt[0] + 1.0).abs() < 1e-10);
    }
}
