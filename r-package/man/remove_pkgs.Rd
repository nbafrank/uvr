\name{remove_pkgs}
\alias{remove_pkgs}
\title{Remove packages from the project}
\usage{
remove_pkgs(packages, dir = NULL, quiet = FALSE)
}
\arguments{
\item{packages}{Character vector of package names to remove.}
\item{dir}{Optional working directory. Defaults to \code{getwd()}.}
\item{quiet}{If \code{TRUE}, suppress output.}
}
\value{
Invisible \code{TRUE} on success.
}
\description{
Removes one or more packages from the manifest and updates the lockfile.
Equivalent to \code{uvr remove} on the command line.
}
\examples{
\dontrun{
remove_pkgs("ggplot2")
remove_pkgs(c("dplyr", "tidyr"))
}
}
