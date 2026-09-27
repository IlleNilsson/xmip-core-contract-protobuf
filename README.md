# xmip-core-contract-protobuf

Protocol Buffers content contract: sound wire format always, walked as a bound message type of a .proto file when a Location names one. A technology of [xmip-core-contract](https://github.com/IlleNilsson/xmip-core-contract).

A `.proto` file is read through `xmip-core-library-codec`'s character
reader: any Unicode whitespace separates tokens, and a name may hold any
letter.

A departure names the field it is at, `shop.v1.Order.lines.qty`, spelled
only when one is raised, and a map entry is read in place rather than built
into a message per entry.

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
