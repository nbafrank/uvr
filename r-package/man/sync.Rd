\name{sync}
\alias{sync}
\title{Synchronize project library from lockfile}
\usage{
sync(frozen = FALSE, dir = NULL, quiet = FALSE)
}
\arguments{
\item{frozen}{If \code{TRUE}, fail if the lockfile is out of date (CI mode).}
\item{dir}{Optional working directory. Defaults to \code{getwd()}.}
\item{quiet}{If \code{TRUE}, suppress output.}
}
\value{
Invisible \code{TRUE} on success.
}
\description{
Installs all packages specified in \code{uvr.lock}. Idempotent --- skips
packages that are already installed. Equivalent to \code{uvr sync}.
}
\examples{
\dontrun{
sync()
sync(frozen = TRUE)  # CI mode: fail if lockfile is stale
}
}
