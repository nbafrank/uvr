\name{install_uvr}
\alias{install_uvr}
\title{Install the uvr binary}
\usage{
install_uvr(method = c("auto", "binary", "cargo"), force = FALSE)
}
\arguments{
\item{method}{Installation method: \code{"auto"} (default) tries a pre-built
GitHub release binary first, then falls back to building from source via
\code{cargo install}. \code{"binary"} downloads a pre-built binary only.
\code{"cargo"} builds from source only.}
\item{force}{If \code{TRUE}, reinstall even if uvr is already present.}
}
\value{
Invisible path to the installed binary.
}
\description{
Downloads and installs the \code{uvr} binary. By default, tries to download
a pre-built binary from GitHub releases. If none is available for the
current platform, falls back to building from source via \code{cargo install}
(requires the Rust toolchain).
}
\examples{
\dontrun{
# Auto-detect best method
install_uvr()

# Force rebuild from source
install_uvr(method = "cargo", force = TRUE)
}
}
