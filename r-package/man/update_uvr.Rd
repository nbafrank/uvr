\name{update_uvr}
\alias{update_uvr}
\title{Update uvr (R package + CLI binary)}
\usage{
update_uvr(ref = "HEAD", method = "auto", quiet = FALSE)
}
\arguments{
\item{ref}{Git ref (branch, tag, commit SHA) to install the R package from.
Defaults to the default branch (\code{"HEAD"}).}
\item{method}{Binary install method. See \code{\link{install_uvr}} for the
full set; defaults to \code{"auto"} (pre-built binary when available,
cargo build otherwise).}
\item{quiet}{If \code{TRUE}, suppress progress messages.}
}
\value{
Invisible named list with \code{r_package} (installed version or \code{NA}
on failure) and \code{binary} (path to the CLI binary).
}
\description{
Convenience helper that updates both the \code{uvr} R companion package
(from GitHub) and the underlying \code{uvr} CLI binary in one call. The
two components are released together, and forgetting to update one after
the other is a common source of version-skew bugs.
}
\details{
The function does \emph{not} automatically restart your R session. After it
completes, run \code{rstudioapi::restartSession()} (RStudio / Positron), or
simply restart R to load the newly installed package.
}
\examples{
\dontrun{
update_uvr()
# then restart R to pick up the new package
}
}
