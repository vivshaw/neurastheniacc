use anyhow::{Context, Result, ensure};
use wasmi::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc};

pub struct Wasm {
    engine: Engine,
    module: Module,
    export: String,
}

type Instance = (Store<StoreLimits>, TypedFunc<(i32, i32), i32>);

impl Wasm {
    pub fn new(bytes: &[u8], export: &str) -> Result<Self> {
        ensure!(bytes.len() <= 1_048_576, "module exceeds 1 MiB");
        for payload in wasmparser::Parser::new(0).parse_all(bytes) {
            if let wasmparser::Payload::GlobalSection(globals) = payload? {
                ensure!(
                    globals.count() == 0,
                    "globals are not allowed in pure functions"
                );
            }
        }
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, bytes).context("invalid Wasm")?;
        ensure!(module.imports().next().is_none(), "imports are not allowed");
        let this = Self {
            engine,
            module,
            export: export.into(),
        };
        this.instance()?;
        Ok(this)
    }

    fn instance(&self) -> Result<Instance> {
        let limits = StoreLimitsBuilder::new()
            .instances(1)
            .memories(0)
            .tables(0)
            .build();
        let mut store = Store::new(&self.engine, limits);
        store.limiter(|limits| limits);
        store.set_fuel(100_000)?;
        let instance = Linker::new(&self.engine)
            .instantiate(&mut store, &self.module)?
            .ensure_no_start(&mut store)
            .context("start functions are not allowed")?;
        let func = instance
            .get_typed_func::<(i32, i32), i32>(&store, &self.export)
            .context("expected exported function (i32, i32) -> i32")?;
        Ok((store, func))
    }

    pub fn run(&self, x: u32, y: u32) -> Result<u32> {
        // Fresh state per input prevents one test from contaminating another.
        let (mut store, func) = self.instance()?;
        Ok(func.call(&mut store, (x as i32, y as i32))? as u32)
    }
}
