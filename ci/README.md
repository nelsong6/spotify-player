# Shared CI

The repository selects the Rust, release and cluster-service suites with Forgejo
repository topics. `.ci.json` declares the platform features, locked fzt dependency,
joined release targets and Helm render. The default suite checks workflow rules
and dependency fetches.

The root Dockerfile packages the assembled Linux session binary. Its terminating
`--self-test` checks the binary in the final Debian image, including dynamic linking,
and verifies the Snapcast and staging tools without connecting to Spotify or the
live session.
