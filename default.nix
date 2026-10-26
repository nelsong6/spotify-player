{
  lib,
  rustPlatform,
  pkg-config,
  openssl,
  cmake,
  installShellFiles,
  writableTmpDirAsHomeHook,

  # the cast's audio output
  alsa-lib,
  dbus,
  fontconfig,
  libsixel,
  autoconf,
  automake,
  libtool,

  # build options
  withStreaming ? true,
  withCast ? true,
  withMediaControl ? true,
  withImage ? true,
  withNotify ? true,
  withSixel ? true,
  withFuzzy ? true,
  stdenv,
  makeBinaryWrapper,
}:

rustPlatform.buildRustPackage rec {
  pname = "spotify-player";
  version =
    let
      toml = builtins.fromTOML (builtins.readFile ./spotify_player/Cargo.toml);
    in
    toml.package.version;

  src = ./.;

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    cmake
    rustPlatform.bindgenHook
    installShellFiles
    autoconf
    automake
    libtool
    # Tries to access $HOME when installing shell files, and on Darwin
    writableTmpDirAsHomeHook
  ]
  ++ lib.optionals stdenv.hostPlatform.isDarwin [
    makeBinaryWrapper
  ];

  buildInputs = [
    openssl
    dbus
    fontconfig
  ]
  ++ lib.optionals withSixel [ libsixel ]
  ++ lib.optionals (withCast && stdenv.hostPlatform.isLinux) [ alsa-lib ];

  buildNoDefaultFeatures = true;

  buildFeatures =
    [ ]
    ++ lib.optionals withCast [ "cast" ]
    ++ lib.optionals withMediaControl [ "media-control" ]
    ++ lib.optionals withImage [ "image" ]
    ++ lib.optionals withNotify [ "notify" ]
    ++ lib.optionals withStreaming [ "streaming" ]
    ++ lib.optionals withSixel [ "sixel" ]
    ++ lib.optionals withFuzzy [ "fzf" ];

  postInstall =
    let
      inherit (lib.strings) optionalString;
    in
    # sixel-sys is dynamically linked to libsixel
    optionalString (stdenv.hostPlatform.isDarwin && withSixel) ''
      wrapProgram $out/bin/spotify_player \
        --prefix DYLD_LIBRARY_PATH : "${lib.makeLibraryPath [ libsixel ]}"
    ''
    + optionalString (stdenv.buildPlatform.canExecute stdenv.hostPlatform) ''
      installShellCompletion --cmd spotify_player \
        --bash <($out/bin/spotify_player generate bash) \
        --fish <($out/bin/spotify_player generate fish) \
         --zsh <($out/bin/spotify_player generate zsh)
    '';

  meta = {
    description = "Terminal spotify player that has feature parity with the official client";
    homepage = "https://github.com/aome510/spotify-player";
    changelog = "https://github.com/aome510/spotify-player/releases/tag/v${version}";
    mainProgram = "spotify_player";
    license = lib.licenses.mit;
  };
}
