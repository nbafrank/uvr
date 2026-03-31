#' Initialize a new uvr project
#'
#' Creates a \code{uvr.toml} manifest and \code{.uvr/library/} directory.
#' Equivalent to \code{uvr init} on the command line.
#'
#' @param name Optional project name. Defaults to the current directory name.
#' @param r_version Optional R version constraint, e.g. \code{">=4.3.0"}.
#' @param dir Optional working directory. Defaults to \code{getwd()}.
#' @param quiet If \code{TRUE}, suppress output.
#' @return Invisible \code{TRUE} on success.
#' @export
init <- function(name = NULL, r_version = NULL, dir = NULL, quiet = FALSE) {
  args <- "init"
  if (!is.null(name)) args <- c(args, name)
  if (!is.null(r_version)) args <- c(args, "--r-version", r_version)
  run_uvr(args, dir = dir, quiet = quiet)
}
