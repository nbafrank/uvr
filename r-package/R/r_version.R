#' Install an R version
#'
#' Downloads and installs a specific R version to \code{~/.uvr/r-versions/}.
#' Equivalent to \code{uvr r install <version>} on the command line.
#'
#' @param version R version string, e.g. \code{"4.4.2"}.
#' @param quiet If \code{TRUE}, suppress output.
#' @return Invisible \code{TRUE} on success.
#' @export
r_install <- function(version, quiet = FALSE) {
  run_uvr(c("r", "install", version), quiet = quiet)
}

#' List R versions
#'
#' Shows installed R versions. Use \code{all = TRUE} to show all available
#' versions from CRAN.
#' Equivalent to \code{uvr r list} on the command line.
#'
#' @param all If \code{TRUE}, show all available versions (not just installed).
#' @param quiet If \code{TRUE}, suppress output.
#' @return Invisible \code{TRUE} on success.
#' @export
r_list <- function(all = FALSE, quiet = FALSE) {
  args <- c("r", "list")
  if (isTRUE(all)) args <- c(args, "--all")
  run_uvr(args, quiet = quiet)
}

#' Set the R version constraint
#'
#' Sets the R version constraint in \code{uvr.toml}.
#' Equivalent to \code{uvr r use <version>} on the command line.
#'
#' @param version Version constraint, e.g. \code{">=4.3.0"} or \code{"4.4.2"}.
#' @param dir Optional working directory. Defaults to \code{getwd()}.
#' @param quiet If \code{TRUE}, suppress output.
#' @return Invisible \code{TRUE} on success.
#' @export
r_use <- function(version, dir = NULL, quiet = FALSE) {
  run_uvr(c("r", "use", version), dir = dir, quiet = quiet)
}

#' Pin the R version
#'
#' Writes an exact R version to the \code{.r-version} file.
#' Equivalent to \code{uvr r pin <version>} on the command line.
#'
#' @param version Exact R version, e.g. \code{"4.4.2"}.
#' @param dir Optional working directory. Defaults to \code{getwd()}.
#' @param quiet If \code{TRUE}, suppress output.
#' @return Invisible \code{TRUE} on success.
#' @export
r_pin <- function(version, dir = NULL, quiet = FALSE) {
  run_uvr(c("r", "pin", version), dir = dir, quiet = quiet)
}
