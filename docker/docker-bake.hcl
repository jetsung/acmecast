## https://docs.docker.com/build/bake/
## https://docs.docker.com/reference/cli/docker/buildx/bake/#set
## https://github.com/crazy-max/buildx#remote-with-local
## https://github.com/docker/metadata-action

variable "VERSION" {
  default = "latest"
}

## Special target: https://github.com/docker/metadata-action#bake-definition
target "docker-metadata-action" {}

target "_image" {
    inherits = ["docker-metadata-action"]
}

target "_common" {
    labels = {
        "org.opencontainers.image.authors" = "Jetsung Chan<i@jetsung.com>"
    }
    context = "."
    dockerfile = "docker/Dockerfile"
    platforms = ["linux/amd64"]
    args = {
      VERSION = "${VERSION}"
    }
    no-cache = true
    attest = [
      "type=provenance,disabled=true",
      "type=sbom,disabled=true",
    ]
}

target "default" {
    inherits = ["_common"]
    tags = [
      "app:local",
    ]
}

group "dev" {
  targets = ["dev-amd64", "dev-arm64"]
}

target "dev" {
    inherits = ["_common", "_image"]
}

target "dev-amd64" {
    inherits = ["_common", "_image"]
    platforms = ["linux/amd64"]
}

target "dev-arm64" {
    inherits = ["_common", "_image"]
    platforms = ["linux/arm64"]
}

group "release" {
  targets = ["release"]
}

target "release" {
    inherits = ["_common", "_image"]
    platforms = ["linux/amd64","linux/arm64"]
}
