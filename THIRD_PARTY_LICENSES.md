# Third-party code and acknowledgements

## Ported code

**greenlight-native** — <https://github.com/unknownskl/greenlight-native> — MIT License,
Copyright (c) UnknownSKL (Jim Kroon).
The native Nano V3 transport in `rust/xbox-solo-audio/src/nano/` (SRTP key
derivation, packet layouts, channel handshake) is a Rust port of its TypeScript
implementation, and the packet captures in `rust/xbox-solo-audio/pruebas/nano/`
and their test key come from its test data. Its MIT license applies to those parts:

> Permission is hereby granted, free of charge, to any person obtaining a copy of
> this software and associated documentation files (the "Software"), to deal in the
> Software without restriction, including without limitation the rights to use,
> copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the
> Software, and to permit persons to whom the Software is furnished to do so,
> subject to the following conditions: The above copyright notice and this
> permission notice shall be included in all copies or substantial portions of the
> Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND.

## Protocol references (no code copied)

- **Greenlight** — <https://github.com/unknownskl/greenlight> (MIT): the xHome
  session API and the data-channel formats.
- **xal-node** — <https://github.com/unknownskl/xal-node> (MIT): the sequence of
  Microsoft / Xbox Live authentication requests, reimplemented in `auth.rs`.
- **OpenXbox** documentation and **xcloud-rs** — <https://github.com/OpenXbox>:
  Nano protocol documentation used as reference.

## Linked libraries

- **libopus** 1.5.2 — <https://opus-codec.org/> — BSD 3-Clause, Copyright Xiph.Org
  Foundation and contributors; statically linked.
- Rust crates (cpal, tokio, reqwest, serde_json, aes-gcm, …) under their own
  MIT and/or Apache-2.0 licenses; see `rust/Cargo.lock` for the full list.
