# 0.1.0-alpha.2 - Mar. 6, 2024
This is the third alpha release of `lightning-liquidity`. It features
a number of bug fixes and performance improvements over the previous release.

Notably, it introduces service-side payment sequencing in LSPS2, ensuring we'll
only have one intercepted payment in-flight at any given point in time, which
allows the LSP to keep deducting the channel opening fee from the intercepted
payments until one actually succeeds. Moreover, this release fixes a previously
introduced deadlock when being unable to parse a received message.

**Note:** This release is still considered experimental, should not be run in
production, and no compatibility guarantees are given until the release of 0.1.0.

# 0.1.0-alpha.1 - Feb. 28, 2024
This is the second alpha release of `lightning-liquidity`. It features
a number of bug fixes and performance improvements over the previous release.

**Note:** This release is still considered experimental, should not be run in
production, and no compatibility guarantees are given until the release of 0.1.0.

# 0.1.0-alpha - Feb. 15, 2024
This is the first alpha release of `lightning-liquidity`. It features
early-stage client- and service-side support for the LSPS2 just-in-time (JIT)
channel protocol.

**Note:** This release is still considered experimental, should not be run in
production, and no compatibility guarantees are given until the release of 0.1.0.
