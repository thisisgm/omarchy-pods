{ lib, rustPlatform, makeWrapper, pipewire }:

let
  root = ../..;
  manifest = builtins.fromTOML (builtins.readFile (root + "/crates/daemon/Cargo.toml"));
in
rustPlatform.buildRustPackage {
  pname = "pods";
  inherit (manifest.package) version;
  src = lib.cleanSource root;
  cargoLock.lockFile = ../../Cargo.lock;
  cargoBuildFlags = [ "-p" "pods" ];

  nativeBuildInputs = [ makeWrapper ];
  buildInputs = [ pipewire ];
  # The integration tests need a writable D-Bus and PipeWire runtime and seccomp notifications.
  doCheck = false;
  doInstallCheck = true;

  postInstall = ''
    wrapProgram "$out/bin/pods" --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath [ pipewire ]}
    ln -s pods "$out/bin/pods-ctl"
    install -Dm644 packaging/systemd/podsd.service "$out/share/systemd/user/podsd.service"
    substituteInPlace "$out/share/systemd/user/podsd.service" \
      --replace-fail '%h/.local/bin/pods' "$out/bin/pods"
  '';

  installCheckPhase = ''
    runHook preInstallCheck
    test "$("$out/bin/pods" --version)" = "pods ${manifest.package.version}"
    test "$(readlink "$out/bin/pods-ctl")" = pods
    ("$out/bin/pods-ctl" 2>&1 || true) | grep -F "Usage: pods-ctl <command>"
    test -f "$out/share/systemd/user/podsd.service"
    runHook postInstallCheck
  '';

  meta = {
    description = "AirPods control and microphone capture for Linux";
    homepage = "https://github.com/superultrainc/pods";
    license = lib.licenses.mit;
    mainProgram = "pods";
    platforms = [ "x86_64-linux" "aarch64-linux" ];
  };
}
