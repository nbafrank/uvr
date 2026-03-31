skip_if_no_uvr <- function() {
  skip_if_not(
    nzchar(Sys.which("uvr")) ||
      file.exists(file.path(Sys.getenv("HOME"), ".cargo", "bin", "uvr")),
    message = "uvr binary not found"
  )
}

test_that("find_uvr returns a path when uvr is installed", {
  skip_if_no_uvr()
  path <- uvr:::find_uvr()
  expect_true(file.exists(path))
})

test_that("find_uvr caches result", {
  skip_if_no_uvr()
  path1 <- uvr:::find_uvr()
  path2 <- uvr:::find_uvr()
  expect_identical(path1, path2)
})

test_that("run_uvr errors on bad command", {
  skip_if_no_uvr()
  expect_error(uvr:::run_uvr("--nonexistent-flag"), "exited with code")
})

test_that("add errors on empty packages", {
  expect_error(uvr::add(character(0)), "non-empty")
})

test_that("remove_pkgs errors on empty packages", {
  expect_error(uvr::remove_pkgs(character(0)), "non-empty")
})

test_that("run requires script argument", {
  expect_error(uvr::run(), "missing")
})
