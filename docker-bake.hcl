// buildx bake targets for the foundry docker image.
//
// Driven by the shared `docker-buildx-bake-ng` pipeline (see
// .github/workflows/build-docker.yaml), which selects a target via
// `bake_file_target` and injects tags/labels through docker `metadata-action`.
// Also usable locally: `docker buildx bake foundry`.

// Set to "true" automatically by the shared pipeline / CI.
variable "CI" { default = "false" }

// Version/commit stamping. The pipeline exports these as env vars in its
// pre_build_command; buildx bake reads them into the like-named variables.
variable "IMAGE_VERSION"  { default = "dev" }
variable "GIT_COMMIT_HASH" { default = "ffffffffffffffffffffffffffffffffffffffff" }

// Placeholder the docker metadata-action inherits from in CI to inject tags.
target "docker-metadata-action" {}

target "foundry-meta-target" {
  context    = "."
  dockerfile = "Dockerfile"
  target     = "runtime"

  // Multi-arch only in CI (built natively via the pipeline's buildkit pods).
  // Local bake stays single-arch and loads into the docker image store.
  platforms = CI ? ["linux/amd64", "linux/arm64"] : []
  output    = CI ? [] : ["type=docker"]

  // Overridden by metadata-action in CI; a sane default for local builds.
  tags = ["foundry:latest"]

  // Optional private-dependency auth; consumed by the Dockerfile build steps.
  // Sourced from the GITHUB_TOKEN env var the build workflow exports.
  secret = [
    "id=github_token,env=GITHUB_TOKEN",
  ]

  args = {
    RUST_PROFILE   = "dist"
    RUST_FEATURES  = "aws-kms,gcp-kms,turnkey,cli,asm-keccak,js-tracer"
    TAG_NAME       = IMAGE_VERSION
    VERGEN_GIT_SHA = GIT_COMMIT_HASH
  }
}

target "foundry" {
  inherits = ["foundry-meta-target", "docker-metadata-action"]
}

group "default" {
  targets = ["foundry"]
}
