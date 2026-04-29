\name{uvr-package}
\alias{uvr-package}
\alias{uvr}
\docType{package}
\title{R Interface to the 'uvr' Package Manager}
\description{
Provides R functions that wrap the \code{uvr} command-line tool for fast,
reproducible R package management. Use uvr to manage project dependencies
with a lockfile, similar to how \code{uv} works for Python.

\strong{Main functions:}
\describe{
  \item{\code{\link{init}}}{Initialize a new uvr project}
  \item{\code{\link{add}}}{Add packages to the project}
  \item{\code{\link{remove_pkgs}}}{Remove packages from the project}
  \item{\code{\link{sync}}}{Install all packages from the lockfile}
  \item{\code{\link{lock}}}{Re-resolve dependencies and update the lockfile}
  \item{\code{\link{run}}}{Run an R script in the project environment}
  \item{\code{\link{install_uvr}}}{Install the uvr CLI binary}
  \item{\code{\link{update_uvr}}}{Update both R package and CLI binary in one call}
  \item{\code{\link{r_install}}, \code{\link{r_list}}, \code{\link{r_use}}, \code{\link{r_pin}}}{Manage R versions}
}

\strong{Typical workflow:}
\preformatted{
library(uvr)

# Start a new project
init()

# Add packages
add("tidyverse")
add("DESeq2", bioc = TRUE)

# After pulling changes, sync the library
sync()

# Upgrade all packages
lock(upgrade = TRUE)
sync()
}
}
\seealso{
\url{https://github.com/nbafrank/uvr}
}
