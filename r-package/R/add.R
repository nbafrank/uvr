#' Add packages to the project
#'
#' Adds one or more packages to the manifest, updates the lockfile, and
#' installs them. Equivalent to \code{uvr add} on the command line.
#'
#' @param packages Character vector of package names (e.g. \code{"ggplot2"},
#'   \code{"tidymodels@@>=1.0.0"}, \code{"user/repo@@main"}).
#' @param dev If \code{TRUE}, add as dev dependencies.
#' @param bioc If \code{TRUE}, packages come from Bioconductor.
#' @param dir Optional working directory. Defaults to \code{getwd()}.
#' @param quiet If \code{TRUE}, suppress output.
#' @return Invisible \code{TRUE} on success.
#' @export
add <- function(packages, dev = FALSE, bioc = FALSE, dir = NULL, quiet = FALSE) {
  if (length(packages) == 0L) {
    stop("'packages' must be a non-empty character vector.", call. = FALSE)
  }
  args <- c("add", packages)
  if (isTRUE(dev)) args <- c(args, "--dev")
  if (isTRUE(bioc)) args <- c(args, "--bioc")
  run_uvr(args, dir = dir, quiet = quiet)
}
