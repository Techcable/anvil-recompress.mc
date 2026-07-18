# anvil-recompress.mc
A command line tool to recompress Minecraft region files (using the anvil format).

Uses [fastanvil](https://docs.rs/fastanvil/latest/fastanvil/) to read & write files.

Install via `cargo install anvil-recompress`

Functionality is available as a library using the `anvil-recompress-engine` crate.

## Tips
If using uncompressed region files, passing `--long` option to zstd significantly improves compression of the resulting tarfiles.
Without this option, the overall compression of the tarfiles is inferior to per-chunk zlib compression.
With this option it is better than per-chunk zlib by a factor of up to 2x.

## License
Licensed under the [Apache 2.0 License](./LICENSE.txt).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be licensed as above, without any additional terms or conditions.
