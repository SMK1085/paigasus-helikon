# paigasus-helikon-macros

Procedural macros for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents.

Two macros:

- **`#[tool]`** — an attribute macro on an `async fn` that synthesizes an `impl Tool<Ctx>` against [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core). The function's doc comment becomes the tool description the model sees; the argument struct's field docs become the JSON-Schema field descriptions.
- **`tools!`** — a function-like macro that boxes a heterogeneous list of tool values into `Vec<Arc<dyn Tool<Ctx>>>` for an agent builder.

## Install

```bash
cargo add paigasus-helikon-macros
```

Most users enable the `macros` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports both macros as `paigasus_helikon::tool` and `paigasus_helikon::tools`. The macros expand against `paigasus-helikon-core` types, so that crate must also be in scope (the facade brings it in automatically).

## Example

```rust
use paigasus_helikon::core::{ToolContext, ToolError};
use paigasus_helikon::{tool, tools};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, JsonSchema)]
struct AddArgs {
    a: i64,
    b: i64,
}

#[derive(Serialize, JsonSchema)]
struct AddOut {
    sum: i64,
}

/// Adds two numbers.
#[tool]
async fn add(_ctx: &ToolContext<()>, args: AddArgs) -> Result<AddOut, ToolError> {
    Ok(AddOut { sum: args.a + args.b })
}

// Pass to an agent builder with `.tools(...)`:
let my_tools = tools![add];
```

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-macros)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [tools](https://smk1085.github.io/paigasus-helikon/concepts/tools.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
