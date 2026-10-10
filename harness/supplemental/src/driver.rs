//! Fallible supplemental ABI adapter. The legacy adapter is deliberately untouched.
use crate::model::*;
use std::collections::BTreeMap;
use wasmtime::{Config, Engine, ExternType, Instance, Linker, Memory, Module, Store,
    StoreLimits, StoreLimitsBuilder, Trap, WasmParams, WasmResults};

type DResult<T> = Result<T, Failure>;
fn failure(kind: FailureKind, op: &str, msg: impl std::fmt::Display) -> Failure {
    Failure { kind, operation: op.into(), message: msg.to_string().chars().take(2048).collect() }
}
fn guest(op: &str, err: anyhow::Error) -> Failure {
    let kind = if matches!(err.downcast_ref::<Trap>(), Some(Trap::OutOfFuel)) {
        FailureKind::GuestFuel
    } else { FailureKind::Candidate };
    failure(kind, op, format!("{err:#}"))
}

pub struct Factory { engine: Engine, module: Module, profile: Profile }
impl Factory {
    pub fn new(bytes: &[u8], profile: &Profile) -> DResult<Self> {
        profile.validate().map_err(|e| failure(FailureKind::Instrument, "profile", e))?;
        if bytes.len() > MAX_MODULE { return Err(failure(FailureKind::SafetyLimit, "module", "module too large")); }
        if !bytes.starts_with(b"\0asm\x01\0\0\0") { return Err(failure(FailureKind::Candidate, "module", "expected binary WebAssembly v1")); }
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|e| failure(FailureKind::Instrument, "engine", e))?;
        let module = Module::new(&engine, bytes).map_err(|e| guest("compile", e))?;
        Ok(Self { engine, module, profile: profile.clone() })
    }
    fn instantiate(&self) -> DResult<Driver> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.profile.memory_bytes)
            .table_elements(self.profile.table_elements)
            .instances(1).memories(1).tables(8).build();
        let mut store = Store::new(&self.engine, limits);
        store.limiter(|limits| limits);
        store.set_fuel(self.profile.fuel_lifecycle).map_err(|e| failure(FailureKind::Instrument, "fuel", e))?;
        let mut linker = Linker::new(&self.engine);
        for import in self.module.imports() {
            let module = import.module(); let name = import.name();
            let ExternType::Func(ty) = import.ty() else {
                return Err(failure(FailureKind::Candidate, "link", "module must own its memory/tables/globals"));
            };
            // Deterministic compatibility shims, not host OS access.
            let result: anyhow::Result<()> = if name == "emscripten_notify_memory_growth" {
                linker.func_wrap(module, name, |_: i32| {}).map(|_| ())
            } else if module == "wasi_snapshot_preview1" && name == "random_get" {
                linker.func_wrap(module, name, |mut c: wasmtime::Caller<'_, StoreLimits>, p: i32, n: i32| -> i32 {
                    zero_fill(&mut c, p, n)
                }).map(|_| ())
            } else if module == "wasi_snapshot_preview1" && name == "clock_time_get" {
                linker.func_wrap(module, name, |mut c: wasmtime::Caller<'_, StoreLimits>, _: i32, _: i64, p: i32| -> i32 {
                    zero_fill(&mut c, p, 8)
                }).map(|_| ())
            } else if module == "wasi_snapshot_preview1" && name == "clock_res_get" {
                linker.func_wrap(module, name, |mut c: wasmtime::Caller<'_, StoreLimits>, _: i32, p: i32| -> i32 {
                    zero_fill(&mut c, p, 8)
                }).map(|_| ())
            } else if module == "wasi_snapshot_preview1" && name == "environ_sizes_get" {
                linker.func_wrap(module, name, |mut c: wasmtime::Caller<'_, StoreLimits>, p: i32, n: i32| -> i32 {
                    let a = zero_fill(&mut c, p, 4); let b = zero_fill(&mut c, n, 4);
                    if a != 0 { a } else { b }
                }).map(|_| ())
            } else if module == "wasi_snapshot_preview1" && name == "environ_get" {
                linker.func_wrap(module, name, |_: i32, _: i32| -> i32 { 0 }).map(|_| ())
            } else {
                let message = format!("forbidden host call {module}::{name}");
                linker.func_new(module, name, ty, move |_, _, _| { anyhow::bail!("{message}") }).map(|_| ())
            };
            result.map_err(|e| guest("link", e))?;
        }
        let instance = linker.instantiate(&mut store, &self.module).map_err(|e| guest("instantiate", e))?;
        let memory = instance.get_memory(&mut store, "memory")
            .ok_or_else(|| failure(FailureKind::Candidate, "resolve", "missing exported memory"))?;
        let mut driver = Driver { instance, store, memory, profile: self.profile.clone(), calls: Vec::new(), rom_pointer: None };
        // Type-check all ten exports, even those not used by the selected probe.
        for name in ["emu_init", "emu_rom_buffer", "emu_reset", "emu_framebuffer", "emu_audio_buffer", "emu_audio_samples", "emu_audio_rate"] {
            driver.resolve::<(), i32>(name)?;
        }
        driver.resolve::<i32, i32>("emu_load_rom")?;
        driver.resolve::<i32, ()>("emu_set_keys")?;
        driver.resolve::<(), ()>("emu_run_frame")?;
        Ok(driver)
    }
    pub fn run(&self, id: &str, rom: &[u8], schedule: Vec<Action>) -> Trace {
        let mut trace = Trace { id: id.into(), schedule, calls: Vec::new(), captures: BTreeMap::new(), failure: None };
        let mut driver = match self.instantiate() {
            Ok(d) => d,
            Err(e) => { trace.failure = Some(e); return trace; }
        };
        let result = (|| -> DResult<()> {
            if driver.instance.get_func(&mut driver.store, "_initialize").is_some() {
                driver.call::<(), ()>("_initialize", (), vec![], true)?;
            }
            let ok = driver.call::<(), i32>("emu_init", (), vec![], true)?;
            driver.success("emu_init", ok)?;
            for action in &trace.schedule {
                match action {
                    Action::Load => driver.load(rom)?,
                    Action::Reset => {
                        let ok = driver.call::<(), i32>("emu_reset", (), vec![], true)?;
                        driver.success("emu_reset", ok)?;
                    }
                    Action::Keys { value } => { driver.call::<i32, ()>("emu_set_keys", *value as i32, vec![*value as i64], false)?; }
                    Action::Frames { count } => {
                        for _ in 0..*count { driver.call::<(), ()>("emu_run_frame", (), vec![], false)?; }
                    }
                    Action::Frame { tag } => {
                        let capture = driver.frame()?;
                        if trace.captures.insert(tag.clone(), capture).is_some() {
                            return Err(failure(FailureKind::Instrument, "capture", "duplicate capture tag"));
                        }
                    }
                    Action::Audio { tag } => {
                        let capture = driver.audio()?;
                        if trace.captures.insert(tag.clone(), capture).is_some() {
                            return Err(failure(FailureKind::Instrument, "capture", "duplicate capture tag"));
                        }
                    }
                }
            }
            Ok(())
        })();
        trace.calls = driver.calls;
        trace.failure = result.err();
        trace
    }
}

struct Driver {
    instance: Instance, store: Store<StoreLimits>, memory: Memory,
    profile: Profile, calls: Vec<Call>, rom_pointer: Option<u32>,
}
impl Driver {
    fn resolve<P: WasmParams, R: WasmResults>(&mut self, name: &str) -> DResult<()> {
        self.instance.get_typed_func::<P, R>(&mut self.store, name)
            .map(|_| ()).map_err(|e| guest(name, e))
    }
    fn call<P: WasmParams, R: WasmResults + std::fmt::Debug>(
        &mut self, name: &str, params: P, arguments: Vec<i64>, lifecycle: bool,
    ) -> DResult<R> {
        let budget = if lifecycle { self.profile.fuel_lifecycle } else { self.profile.fuel_per_call };
        self.store.set_fuel(budget).map_err(|e| failure(FailureKind::Instrument, name, e))?;
        let result = self.instance.get_typed_func::<P, R>(&mut self.store, name)
            .and_then(|func| func.call(&mut self.store, params)).map_err(|e| guest(name, e));
        let after = self.store.get_fuel().map_err(|e| failure(FailureKind::Instrument, name, e))?;
        self.calls.push(Call {
            name: name.into(), arguments,
            returned: result.as_ref().ok().map(|v| format!("{v:?}")), failure: result.as_ref().err().cloned(),
            fuel_before: budget, fuel_after: after,
        });
        result
    }
    fn success(&self, name: &str, value: i32) -> DResult<()> {
        if value == 1 { Ok(()) } else { Err(failure(FailureKind::Candidate, name, format!("expected success=1, got {value}"))) }
    }
    fn load(&mut self, rom: &[u8]) -> DResult<()> {
        if rom.is_empty() || rom.len() > MAX_ROM { return Err(failure(FailureKind::Instrument, "rom", "invalid host ROM size")); }
        let pointer = self.call::<(), i32>("emu_rom_buffer", (), vec![], false)? as u32;
        if self.rom_pointer.is_some_and(|p| p != pointer) {
            return Err(failure(FailureKind::Candidate, "emu_rom_buffer", "ROM buffer pointer changed"));
        }
        self.rom_pointer = Some(pointer);
        let end = (pointer as usize).checked_add(MAX_ROM)
            .ok_or_else(|| failure(FailureKind::Candidate, "emu_rom_buffer", "range overflow"))?;
        if end > self.memory.data_size(&self.store) {
            return Err(failure(FailureKind::Candidate, "emu_rom_buffer", "documented 32 MiB ROM buffer is not accessible"));
        }
        self.memory.write(&mut self.store, pointer as usize, rom).map_err(|e| failure(FailureKind::Candidate, "rom_copy", e))?;
        let ok = self.call::<i32, i32>("emu_load_rom", rom.len() as i32, vec![rom.len() as i64], true)?;
        self.success("emu_load_rom", ok)
    }
    fn bytes(&mut self, pointer: u32, len: usize, name: &str) -> DResult<Vec<u8>> {
        let end = (pointer as usize).checked_add(len)
            .ok_or_else(|| failure(FailureKind::Candidate, name, "range overflow"))?;
        if end > self.memory.data_size(&self.store) { return Err(failure(FailureKind::Candidate, name, "buffer outside linear memory")); }
        let mut bytes = vec![0; len];
        self.memory.read(&self.store, pointer as usize, &mut bytes).map_err(|e| failure(FailureKind::Candidate, name, e))?;
        Ok(bytes)
    }
    fn frame(&mut self) -> DResult<Capture> {
        // Query each capture; the published ABI does not fix this pointer.
        let pointer = self.call::<(), i32>("emu_framebuffer", (), vec![], false)? as u32;
        Ok(Capture { frame_le: Some(self.bytes(pointer, FRAME_BYTES, "emu_framebuffer")?), ..Capture::default() })
    }
    fn audio(&mut self) -> DResult<Capture> {
        let pairs = self.call::<(), i32>("emu_audio_samples", (), vec![], false)?;
        if pairs < 0 { return Err(failure(FailureKind::Candidate, "emu_audio_samples", "negative stereo-pair count")); }
        if pairs as usize > self.profile.max_audio_pairs {
            return Err(failure(FailureKind::SafetyLimit, "emu_audio_samples", "capture exceeds configured safety limit; not a fidelity verdict"));
        }
        let pcm = if pairs == 0 { Vec::new() } else {
            let pointer = self.call::<(), i32>("emu_audio_buffer", (), vec![], false)? as u32;
            let len = (pairs as usize).checked_mul(4)
                .ok_or_else(|| failure(FailureKind::Candidate, "emu_audio_samples", "byte count overflow"))?;
            self.bytes(pointer, len, "emu_audio_buffer")?.chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
        };
        let rate = self.call::<(), i32>("emu_audio_rate", (), vec![], false)?;
        if rate <= 0 { return Err(failure(FailureKind::Candidate, "emu_audio_rate", "nonpositive sample rate")); }
        Ok(Capture { pcm: Some(pcm), sample_rate: Some(rate as u32), ..Capture::default() })
    }
}

fn zero_fill(caller: &mut wasmtime::Caller<'_, StoreLimits>, pointer: i32, len: i32) -> i32 {
    if len < 0 { return 28; }
    let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else { return 28; };
    let start = pointer as u32 as usize;
    let Some(end) = start.checked_add(len as usize) else { return 28; };
    let data = memory.data_mut(caller);
    let Some(range) = data.get_mut(start..end) else { return 28; };
    range.fill(0); 0
}
