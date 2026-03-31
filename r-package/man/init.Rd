\name{init}
\alias{init}
\title{Initialize a new uvr project}
\usage{
init(name = NULL, r_version = NULL, dir = NULL, quiet = FALSE)
}
\arguments{
\item{name}{Optional project name. Defaults to the current directory name.}
\item{r_version}{Optional R version constraint, e.g. \code{">=4.3.0"}.}
\item{dir}{Optional working directory. Defaults to \code{getwd()}.}
\item{quiet}{If \code{TRUE}, suppress output.}
}
\value{
Invisible \code{TRUE} on success.
}
\description{
Creates a \code{uvr.toml} manifest and \code{.uvr/library/} directory.
Equivalent to \code{uvr init} on the command line.
}
