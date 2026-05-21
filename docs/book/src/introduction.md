# Introduction

`paigasus-helikon` is a Rust SDK for building agentic AI systems. It separates slow-moving primitives (types, traits, message protocols) from fast-moving parts (provider SDKs, execution runtimes, tool catalogs), so downstream projects can pick the surface they need without dragging in the rest.

The SDK does not pick a deployment story, a hosting story, or an observability stack for you. Bring your own.

## What's here

This documentation site is published from the [`paigasus-helikon`](https://github.com/SMK1085/paigasus-helikon) repository. It is currently a **scaffold** — the chapter structure is in place, but most pages are stubs. Real content lands page-by-page alongside the corresponding feature tickets.

## What's not yet here

API documentation lives on [docs.rs](https://docs.rs) once the workspace is published. Internal architectural design notes live in Notion until they migrate here. Tracked work lives in Linear under the project **Paigasus Helikon**.
