\name{add}
\alias{add}
\title{Add packages to the project}
\usage{
add(packages, dev = FALSE, bioc = FALSE, dir = NULL, quiet = FALSE)
}
\arguments{
\item{packages}{Character vector of package names (e.g. \code{"ggplot2"},
\code{"tidymodels@>=1.0.0"}, \code{"user/repo@main"}).}
\item{dev}{If \code{TRUE}, add as dev dependencies.}
\item{bioc}{If \code{TRUE}, packages come from Bioconductor.}
\item{dir}{Optional working directory. Defaults to \code{getwd()}.}
\item{quiet}{If \code{TRUE}, suppress output.}
}
\value{
Invisible \code{TRUE} on success.
}
\description{
Adds one or more packages to the manifest, updates the lockfile, and
installs them. Equivalent to \code{uvr add} on the command line.
}
\examples{
\dontrun{
add("ggplot2")
add(c("dplyr", "tidyr"))
add("DESeq2", bioc = TRUE)
add("user/repo", dev = TRUE)
add("ggplot2@>=3.0.0")
}
}
