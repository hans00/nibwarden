{
  lib,
  rustPlatform,
  pkg-config,
  fontconfig,
  freetype,
  libxkbcommon,
  wayland,
  libGL,
  xorg,
}:

let
  cargoToml = lib.importTOML ../../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = "nibwarden";
  inherit (cargoToml.workspace.package) version;

  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../Cargo.toml
      ../../Cargo.lock
      ../../LICENSE
      ../../crates
      ../../packaging
      ../../xtask
    ];
  };

  cargoLock = {
    lockFile = ../../Cargo.lock;
    # The Bitwarden SDK and passkey-rs come from git at the commits Cargo.lock pins.
    allowBuiltinFetchGit = true;
  };

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [
    fontconfig
    freetype
  ];

  # The two profiles `cargo xtask build` uses; nibwarden-probe is a test tool.
  buildPhase = ''
    runHook preBuild
    cargo build --frozen --release -p nibwarden-cli -p nibwarden-daemon
    cargo build --frozen --profile release-gui -p nibwarden-gui
    runHook postBuild
  '';

  # The e2e tests need a Vaultwarden server; the unit tests run in the repository.
  doCheck = false;

  installPhase = ''
    runHook preInstall
    cargo run --frozen -p xtask -- install --no-build --prefix "$out"
    install -Dm644 LICENSE "$out/share/licenses/nibwarden/LICENSE"
    runHook postInstall
  '';

  # winit and the OpenGL renderer load these at run time; NixOS has no global library path.
  postFixup = ''
    patchelf --add-rpath ${
      lib.makeLibraryPath [
        libxkbcommon
        wayland
        libGL
        xorg.libX11
        xorg.libXcursor
        xorg.libXi
        xorg.libXrandr
      ]
    } $out/bin/nibwarden-gui
  '';

  meta = {
    description = "Bitwarden-compatible password manager for the Linux desktop";
    homepage = "https://github.com/hans00/nibwarden";
    license = lib.licenses.gpl3Only;
    platforms = lib.platforms.linux;
    mainProgram = "nibwarden";
  };
}
