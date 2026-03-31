#' Update the lockfile
#'
#' Re-resolves all dependencies and writes \code{uvr.lock} without installing.
#' Equivalent to \code{uvr lock}.
#'
#' @param upgrade If \code{TRUE}, upgrade all packages to their latest allowed
#'   versions.
#' @param dir Optional working directory. Defaults to \code{getwd()}.
#' @param quiet If \code{TRUE}, suppress output.
#' @return Invisible \code{TRUE} on success.
#' @export
lock <- function(upgrade = FALSE, dir = NULL, quiet = FALSE) {
  args <- "lock"
  if (isTRUE(upgrade)) args <- c(args, "--upgrade")
  run_uvr(args, dir = dir, quiet = quiet)
}
