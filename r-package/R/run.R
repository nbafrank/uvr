#' Run an R script in the project environment
#'
#' Executes a script with the project library active.
#' Equivalent to \code{uvr run script.R} on the command line.
#' For interactive R, use the CLI directly: \code{uvr run}.
#'
#' @param script Path to an R script.
#' @param args Character vector of arguments forwarded to the script.
#' @param dir Optional working directory. Defaults to \code{getwd()}.
#' @param quiet If \code{TRUE}, suppress output.
#' @return Invisible \code{TRUE} on success.
#' @export
run <- function(script, args = character(), dir = NULL, quiet = FALSE) {
  cli_args <- c("run", script)
  if (length(args) > 0L) cli_args <- c(cli_args, "--", args)
  run_uvr(cli_args, dir = dir, quiet = quiet)
}
