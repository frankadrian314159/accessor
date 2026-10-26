;;;; A test of Accessor's C interface (accessor.h) from Common Lisp: SBCL and CFFI.
;;;;
;;;; Run with run.sh / run.ps1, or:   sbcl --non-interactive --load accessor-cabi-test.lisp
;;;; Needs Quicklisp (for CFFI) and the native library, built with   cargo build -p accessor-capi
;;;; Set ACCESSOR_LIBRARY to use a library somewhere else.

(require :asdf)
(require :sb-posix)

(let ((setup (merge-pathnames "quicklisp/setup.lisp" (user-homedir-pathname))))
  (unless (find-package :ql)
    (if (probe-file setup)
        (load setup)
        (error "Quicklisp was not found at ~a: it is needed to load CFFI" setup))))
(funcall (intern "QUICKLOAD" :ql) :cffi :silent t)

(defpackage :accessor-cabi-test
  (:use :common-lisp :cffi))
(in-package :accessor-cabi-test)

;;; ---- the library -----------------------------------------------------------------------------

(defparameter *here*
  (or *load-truename* (merge-pathnames "x" (truename *default-pathname-defaults*))))

(defparameter *library*
  (or (let ((env (sb-posix:getenv "ACCESSOR_LIBRARY"))) (and env (plusp (length env)) env))
      (namestring
       (merge-pathnames
        (format nil "../target/debug/~a"
                #+windows "accessor_capi.dll" #+darwin "libaccessor_capi.dylib" #-(or windows darwin) "libaccessor_capi.so")
        *here*))))

(unless (probe-file *library*)
  (format t "native library not found: ~a~%(build it with: cargo build -p accessor-capi)~%" *library*)
  (sb-ext:exit :code 1))

(load-foreign-library *library*)

;;; ---- the interface (accessor.h) --------------------------------------------------------------

(defconstant +ok+ 0)
(defconstant +err-syntax+ 1)
(defconstant +err-catalog+ 2)
(defconstant +err-constraint+ 3)
(defconstant +err-locked+ 8)
(defconstant +err-misuse+ 9)
(defconstant +type-null+ 0)
(defconstant +type-integer+ 1)
(defconstant +type-double+ 2)
(defconstant +type-text+ 3)
(defconstant +type-bool+ 4)
(defconstant +type-decimal+ 5)
(defconstant +type-bignum+ 6)

(defcfun "acc_version" :string)
(defcfun "acc_version_number" :int)
(defcfun "acc_abi_version" :int)
(defcfun "acc_file_format_version" :int)
(defcfun "acc_open" :int (path :pointer) (out :pointer))
(defcfun "acc_close" :void (db :pointer))
(defcfun "acc_errcode" :int (db :pointer))
(defcfun "acc_errmsg" :string (db :pointer))
(defcfun "acc_exec" :int (db :pointer) (sql :pointer) (len :size) (out :pointer))
(defcfun "acc_in_transaction" :int (db :pointer))
(defcfun "acc_prepare" :int (db :pointer) (sql :pointer) (len :size) (out :pointer))
(defcfun "acc_stmt_free" :void (stmt :pointer))
(defcfun "acc_stmt_param_count" :int (stmt :pointer))
(defcfun "acc_stmt_param_name" :string (stmt :pointer) (index :int))
(defcfun "acc_bind_null" :int (stmt :pointer) (index :int))
(defcfun "acc_bind_int64" :int (stmt :pointer) (index :int) (v :int64))
(defcfun "acc_bind_double" :int (stmt :pointer) (index :int) (v :double))
(defcfun "acc_bind_bool" :int (stmt :pointer) (index :int) (v :int))
(defcfun "acc_bind_text" :int (stmt :pointer) (index :int) (text :pointer) (len :size))
(defcfun "acc_bind_decimal" :int (stmt :pointer) (index :int) (text :pointer) (len :size))
(defcfun "acc_bind_bignum" :int (stmt :pointer) (index :int) (text :pointer) (len :size))
(defcfun "acc_stmt_clear_bindings" :int (stmt :pointer))
(defcfun "acc_stmt_run" :int (stmt :pointer) (out :pointer))
(defcfun "acc_result_free" :void (res :pointer))
(defcfun "acc_result_row_count" :size (res :pointer))
(defcfun "acc_result_column_count" :size (res :pointer))
(defcfun "acc_result_affected" :size (res :pointer))
(defcfun "acc_result_column_name" :string (res :pointer) (col :size))
(defcfun "acc_value_type" :int (res :pointer) (row :size) (col :size))
(defcfun "acc_value_int64" :int64 (res :pointer) (row :size) (col :size))
(defcfun "acc_value_double" :double (res :pointer) (row :size) (col :size))
(defcfun "acc_value_text" :pointer (res :pointer) (row :size) (col :size) (len :pointer))

;;; ---- Lisp-side helpers -----------------------------------------------------------------------

(defmacro with-utf8 ((ptr len string) &body body)
  "Binds PTR and LEN (in bytes) to STRING as UTF-8, not NUL-terminated: the interface takes lengths."
  (let ((s (gensym)))
    `(let ((,s ,string))
       (with-foreign-string ((,ptr ,len) ,s :encoding :utf-8 :null-terminated-p nil)
         ,@body))))

(defun open-db (path)
  "Returns (values code db). A handle comes back even when opening fails: it holds the error."
  (with-foreign-object (out :pointer)
    (setf (mem-ref out :pointer) (null-pointer))
    (let ((code (if path
                    (with-foreign-string (p path :encoding :utf-8) (acc-open p out))
                    (acc-open (null-pointer) out))))
      (values code (mem-ref out :pointer)))))

(defun exec (db sql)
  "Runs SQL. Returns (values code result); a result must be freed with ACC-RESULT-FREE."
  (with-utf8 (p n sql)
    (with-foreign-object (out :pointer)
      (setf (mem-ref out :pointer) (null-pointer))
      (let ((code (acc-exec db p n out)))
        (values code (mem-ref out :pointer))))))

(defun text-at (res row col)
  "The value at ROW, COL as a Lisp string (decoded from UTF-8, by its length)."
  (with-foreign-object (len :size)
    (let ((p (acc-value-text res row col len)))
      (if (null-pointer-p p)
          nil
          (foreign-string-to-lisp p :count (mem-ref len :size) :encoding :utf-8)))))

(defmacro with-result ((res db sql) &body body)
  "Runs SQL, which must succeed, and binds RES to its result for the body."
  `(multiple-value-bind (code ,res) (exec ,db ,sql)
     (unless (= code +ok+) (error "~a failed: ~a" ,sql (acc-errmsg ,db)))
     (unwind-protect (progn ,@body) (acc-result-free ,res))))

(defun run! (db sql)
  "Runs SQL for its effect; signals if it fails."
  (multiple-value-bind (code res) (exec db sql)
    (unless (= code +ok+) (error "~a failed: ~a" sql (acc-errmsg db)))
    (unless (null-pointer-p res) (acc-result-free res))
    t))

(defun value-at (res row col)
  "The value at ROW, COL as the Lisp value its type calls for: integers and booleans as integers, floating
point numbers as doubles, NULL as NIL, everything else (text, exact numbers, dates) as the string."
  (let ((type (acc-value-type res row col)))
    (cond ((= type +type-null+) nil)
          ((or (= type +type-integer+) (= type +type-bool+)) (acc-value-int64 res row col))
          ((= type +type-double+) (acc-value-double res row col))
          (t (text-at res row col)))))

(defun one (db sql)
  "The first value of the first row (see VALUE-AT)."
  (with-result (r db sql) (value-at r 0 0)))

(defmacro with-db ((db &optional path) &body body)
  `(multiple-value-bind (code ,db) (open-db ,path)
     (unless (= code +ok+) (error "open failed: ~a" (acc-errmsg ,db)))
     (unwind-protect (progn ,@body) (acc-close ,db))))

;;; ---- a very small test framework -------------------------------------------------------------

(defvar *checks* 0)
(defvar *failures* 0)

(defmacro check (form &optional what)
  `(progn
     (incf *checks*)
     (unless ,form
       (incf *failures*)
       (format t "FAIL  ~a~%" (or ,what ',form)))))

(defmacro check-equal (expected form &optional what)
  `(let ((expected ,expected) (actual ,form))
     (incf *checks*)
     (unless (equal expected actual)
       (incf *failures*)
       (format t "FAIL  ~a: expected ~s, got ~s~%" (or ,what ',form) expected actual))))

(defmacro deftest (name &body body)
  `(progn
     (let ((before *failures*))
       (handler-case (progn ,@body)
         (error (e)
           (incf *checks*)
           (incf *failures*)
           (format t "FAIL  ~a: ~a~%" ,name e)))
       (format t "~a  ~a~%" (if (= before *failures*) "ok  " "FAIL") ,name))))

;;; ---- the tests -------------------------------------------------------------------------------

(deftest "the library says what it is"
  (check-equal "0.1.0" (acc-version) "version string")
  (check-equal 100 (acc-version-number) "version number (major*10000 + minor*100 + patch)")
  (check-equal 1 (acc-abi-version) "ABI version")
  (check-equal 6 (acc-file-format-version) "file format version"))

(deftest "SQL in, rows out"
  (with-db (db)
    (run! db "CREATE TABLE t (id COUNTER PRIMARY KEY, name TEXT, score DOUBLE, ok YESNO, note TEXT)")
    (run! db "INSERT INTO t (name, score, ok) VALUES ('Ada', 91.5, TRUE), ('Grace', 88, FALSE), ('Edsger', 70.25, TRUE)")
    (with-result (r db "SELECT id, name, score, ok, note FROM t ORDER BY id")
      (check-equal 3 (acc-result-row-count r) "rows")
      (check-equal 5 (acc-result-column-count r) "columns")
      (check-equal '("id" "name" "score" "ok" "note")
                   (loop for c below 5 collect (acc-result-column-name r c)) "column names")
      (check-equal +type-integer+ (acc-value-type r 0 0) "id is an integer")
      (check-equal 2 (acc-value-int64 r 1 0) "second id")
      (check-equal +type-text+ (acc-value-type r 0 1))
      (check-equal "Grace" (text-at r 1 1))
      (check-equal +type-double+ (acc-value-type r 2 2))
      (check-equal 70.25d0 (acc-value-double r 2 2))
      (check-equal +type-bool+ (acc-value-type r 0 3))
      (check-equal 1 (acc-value-int64 r 0 3) "TRUE reads as 1")
      (check-equal 0 (acc-value-int64 r 1 3) "FALSE reads as 0")
      (check-equal +type-null+ (acc-value-type r 0 4) "a NULL")
      (check (null (text-at r 0 4)) "NULL text is NULL")
      ;; Outside the result: defaults, not a crash.
      (check-equal +type-null+ (acc-value-type r 99 0))
      (check-equal 0 (acc-value-int64 r 0 99)))
    (with-result (r db "UPDATE t SET score = score + 1")
      (check-equal 3 (acc-result-affected r) "rows changed"))))

(deftest "text is UTF-8 and has lengths"
  (with-db (db)
    (run! db "CREATE TABLE u (s TEXT)")
    (let ((s (format nil "h~(~c~)llo ~c~c ~c" (code-char #xe9) (code-char #x4e16) (code-char #x754c) (code-char #x1f600))))
      (run! db (format nil "INSERT INTO u VALUES ('~a')" s))
      (with-result (r db "SELECT s, LENGTH(s) FROM u")
        (check-equal s (text-at r 0 0) "unicode round trip")
        (check-equal 10 (value-at r 0 1) "LENGTH counts characters, an emoji as one")))))

(deftest "prepared statements and every kind of bind"
  (with-db (db)
    (run! db "CREATE TABLE p (i INTEGER, d DOUBLE, b YESNO, s TEXT, n DECIMAL, big BIGNUM)")
    (multiple-value-bind (code stmt)
        (with-utf8 (sql len "INSERT INTO p VALUES (@i, @d, @b, @s, @n, @big)")
          (with-foreign-object (out :pointer)
            (values (acc-prepare db sql len out) (mem-ref out :pointer))))
      (check-equal +ok+ code "prepare")
      (unwind-protect
           (progn
             (check-equal 6 (acc-stmt-param-count stmt) "six parameters")
             (check-equal "i" (acc-stmt-param-name stmt 0))
             (check-equal "big" (acc-stmt-param-name stmt 5))
             (flet ((run-it ()
                      (with-foreign-object (out :pointer)
                        (let ((code (acc-stmt-run stmt out)))
                          (unless (null-pointer-p (mem-ref out :pointer)) (acc-result-free (mem-ref out :pointer)))
                          code))))
               ;; a run needs every parameter bound
               (check-equal +err-misuse+ (run-it) "unbound parameters are refused")
               (check-equal +ok+ (acc-bind-int64 stmt 0 -42))
               (check-equal +ok+ (acc-bind-double stmt 1 0.5d0))
               (check-equal +ok+ (acc-bind-bool stmt 2 1))
               (with-utf8 (p n "it's text") (check-equal +ok+ (acc-bind-text stmt 3 p n)))
               (with-utf8 (p n "0.1") (check-equal +ok+ (acc-bind-decimal stmt 4 p n)))
               (with-utf8 (p n "123456789012345678901234567890") (check-equal +ok+ (acc-bind-bignum stmt 5 p n)))
               (check-equal +ok+ (run-it) "first run")
               ;; the same statement again, with a NULL
               (check-equal +ok+ (acc-bind-int64 stmt 0 7))
               (check-equal +ok+ (acc-bind-null stmt 3))
               (check-equal +ok+ (run-it) "second run")
               (check-equal +ok+ (acc-stmt-clear-bindings stmt) "bindings cleared")
               (check-equal +err-misuse+ (run-it) "cleared bindings are unbound again")))
        (acc-stmt-free stmt)))
    (with-result (r db "SELECT i, d, b, s, n + 0.2, big FROM p ORDER BY i")
      (check-equal 2 (acc-result-row-count r))
      (check-equal -42 (acc-value-int64 r 0 0))
      (check-equal 0.5d0 (acc-value-double r 0 1))
      (check-equal "it's text" (text-at r 0 3) "a quote in bound text needs no escaping")
      (check-equal +type-decimal+ (acc-value-type r 0 4))
      (check-equal "0.3" (text-at r 0 4) "0.1 + 0.2 is exactly 0.3")
      (check-equal +type-bignum+ (acc-value-type r 0 5))
      (check-equal "123456789012345678901234567890" (text-at r 0 5) "a bignum keeps every digit")
      (check-equal +type-null+ (acc-value-type r 1 3) "the NULL that was bound"))))

(deftest "errors have codes and messages"
  (with-db (db)
    (run! db "CREATE TABLE e (id INTEGER PRIMARY KEY)")
    (run! db "INSERT INTO e VALUES (1)")
    (flet ((code-of (sql) (multiple-value-bind (code r) (exec db sql)
                            (unless (null-pointer-p r) (acc-result-free r))
                            code)))
      (check-equal +err-syntax+ (code-of "SELEC 1") "syntax")
      (check-equal +err-syntax+ (acc-errcode db) "the connection remembers the code")
      (check (plusp (length (acc-errmsg db))) "and a message")
      (check-equal +err-catalog+ (code-of "SELECT * FROM nope") "a missing table")
      (check-equal +err-constraint+ (code-of "INSERT INTO e VALUES (1)") "a duplicate key")
      (check-equal +ok+ (code-of "SELECT * FROM e") "and then success again")
      (check-equal 0 (acc-errcode db) "which clears the error"))))

(deftest "misuse is refused, not a crash"
  (check-equal +err-misuse+ (acc-exec (null-pointer) (null-pointer) 0 (null-pointer)) "NULL handle")
  (with-db (db)
    (with-foreign-object (out :pointer)
      (check-equal +err-misuse+ (acc-exec db (null-pointer) 0 out) "NULL SQL"))))

(deftest "transactions"
  (with-db (db)
    (run! db "CREATE TABLE acct (id INTEGER PRIMARY KEY, bal INTEGER)")
    (run! db "INSERT INTO acct VALUES (1, 100), (2, 100)")
    (check-equal 0 (acc-in-transaction db))
    (run! db "BEGIN")
    (check-equal 1 (acc-in-transaction db) "inside")
    (run! db "UPDATE acct SET bal = 0")
    (run! db "CREATE TABLE gone (x INTEGER)")
    (run! db "ROLLBACK")
    (check-equal 0 (acc-in-transaction db))
    (check-equal 200 (one db "SELECT SUM(bal) FROM acct") "the update was taken back")
    (check-equal +err-catalog+ (multiple-value-bind (c r) (exec db "SELECT * FROM gone")
                                 (unless (null-pointer-p r) (acc-result-free r)) c)
                 "so was the table")
    (run! db "BEGIN")
    (run! db "UPDATE acct SET bal = bal - 30 WHERE id = 1")
    (run! db "UPDATE acct SET bal = bal + 30 WHERE id = 2")
    (run! db "COMMIT")
    (check-equal 70 (one db "SELECT bal FROM acct WHERE id = 1"))
    (check-equal 130 (one db "SELECT bal FROM acct WHERE id = 2"))))

(deftest "a database file persists and is locked while open"
  (let* ((dir (format nil "~a/accessor-cl-test-~a/" (or (sb-posix:getenv "TEMP") (sb-posix:getenv "TMPDIR") "/tmp") (random 1000000000)))
         (path (concatenate 'string dir "test.adb")))
    (ensure-directories-exist dir)
    (unwind-protect
         (progn
           (with-db (db path)
             (run! db "CREATE TABLE kept (n INTEGER)")
             (run! db "INSERT INTO kept VALUES (42)")
             ;; a second open of the same file is refused, and does not disturb the first
             (multiple-value-bind (code second) (open-db path)
               (check-equal +err-locked+ code "the file is locked")
               (check-equal +err-locked+ (acc-errcode second))
               (acc-close second))
             (check-equal 42 (one db "SELECT n FROM kept") "the first connection is undisturbed"))
           (with-db (db path)
             (check-equal 42 (one db "SELECT n FROM kept") "reopened after close")))
      (dolist (f (directory (concatenate 'string dir "*.*")))
        (ignore-errors (delete-file f)))
      (ignore-errors (sb-ext:delete-directory dir)))))

(deftest "a larger result"
  (with-db (db)
    (run! db "CREATE TABLE big (n INTEGER)")
    (run! db "INSERT INTO big SELECT n FROM GENERATE_SERIES(1, 20000) AS g (n)")
    (with-result (r db "SELECT n FROM big ORDER BY n")
      (check-equal 20000 (acc-result-row-count r))
      (check-equal 20000 (acc-value-int64 r 19999 0))
      (check-equal 200010000 (loop for i below 20000 sum (acc-value-int64 r i 0))))))

(format t "~%~d checks, ~d failed~%" *checks* *failures*)
(sb-ext:exit :code (if (zerop *failures*) 0 1))
