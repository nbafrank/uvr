#' Remove packages from the project
#'
#' Removes one or more packages from the manifest and updates the lockfile.
#' Equivalent to \code{uvr remove} on the command line.
#'
#' @param packages Character vector of package names to remove.
#' @param dir Optional working directory. Defaults to \code{getwd()}.
#' @param quiet If \code{TRUE}, suppress output.
#' @return Invisible \code{TRUE} on success.
#' @export
remove_pkgs <- function(packages, dir = NULL, quiet = FALSE) {
  if (length(packages) == 0L) {
    stop("'packages' must be a non-empty character vector.", call. = FALSE)
  }
  run_uvr(c("remove", packages), dir = dir, quiet = quiet)
}
