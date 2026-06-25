# paigasus-helikon-providers-bedrock

Amazon Bedrock (Converse API) provider for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK.

[![crates.io](https://img.shields.io/crates/v/paigasus-helikon-providers-bedrock.svg)](https://crates.io/crates/paigasus-helikon-providers-bedrock)
[![docs.rs](https://docs.rs/paigasus-helikon-providers-bedrock/badge.svg)](https://docs.rs/paigasus-helikon-providers-bedrock)
[![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg)](#license)

Implements the `paigasus-helikon-core` `Model` trait against the AWS Bedrock
Converse streaming API. Supports Anthropic Claude, Amazon Nova/Titan, Meta
Llama, Mistral, Cohere, and AI21 model families with tool use, streaming, and
optional extended thinking.

## Install

```bash
cargo add paigasus-helikon-providers-bedrock
```

Or via the facade:

```bash
cargo add paigasus-helikon --features bedrock
```

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE)
or [MIT license](../../LICENSE-MIT) at your option.
