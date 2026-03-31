\name{lock}
\alias{lock}
\title{Update the lockfile}
\usage{
lock(upgrade = FALSE, dir = NULL, quiet = FALSE)
}
\arguments{
\item{upgrade}{If \code{TRUE}, upgrade all packages to their latest allowed versions.}
\item{dir}{Optional working directory. Defaults to \code{getwd()}.}
\item{quiet}{If \code{TRUE}, suppress output.}
}
\value{
Invisible \code{TRUE} on success.
}
\description{
Re-resolves all dependencies and writes \code{uvr.lock} without installing.
Equivalent to \code{uvr lock}.
}
